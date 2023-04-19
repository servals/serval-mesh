use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{any, get, post};
use axum::Json;
use utils::mesh::ServalRole;
use utils::structs::api::{
    SchedulerEnqueueJobResponse, SchedulerJobClaimResponse, SchedulerJobStatusResponse,
};
use utils::structs::JobStatus;
use uuid::Uuid;

use crate::structures::*;

/// Mount all jobs endpoint handlers onto the passed-in router.
pub fn mount(router: ServalRouter) -> ServalRouter {
    router
        .route("/v1/scheduler/enqueue/:name", post(enqueue_job))
        .route("/v1/scheduler/claim", post(claim_job))
        .route("/v1/scheduler/:job_id/complete", post(complete_job))
        .route("/v1/scheduler/:job_id/status", get(job_status))
        .route("/v1/scheduler/:job_id/tickle", post(tickle_job))
    // todo: route to mark a job as failed
}

/// Mount a handler that relays all job-running requests to another node.
pub fn mount_proxy(router: ServalRouter) -> ServalRouter {
    router.route("/v1/scheduler/*rest", any(proxy))
}

/// Relay all scheduler requests to a node that can handle them.
async fn proxy(State(state): State<AppState>, mut request: Request<Body>) -> impl IntoResponse {
    let path = request.uri().path();
    log::info!("relaying a scheduler request; path={path}");
    metrics::increment_counter!("proxy:scheduler:{path}");

    if let Ok(resp) =
        super::proxy::relay_request(&mut request, &ServalRole::Scheduler, &state.instance_id).await
    {
        resp
    } else {
        // Welp, not much we can do
        metrics::increment_counter!("proxy:error");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Peer with the job runner role not available",
        )
            .into_response()
    }
}

/// This is the main scheduler endpoint. It accepts incoming jobs and holds them until they can be
/// claimed by an appropriate runner.
async fn enqueue_job(
    Path(name): Path<String>,
    input: Bytes,
) -> Result<Json<SchedulerEnqueueJobResponse>, impl IntoResponse> {
    let mut queue = JOBS
        .get()
        .expect("Job queue not initialized")
        .lock()
        .unwrap();
    let Ok(job_id) = queue.enqueue(name, input.to_vec()) else {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, String::from("Failed to enqueue job")).into_response());
    };

    Ok(Json(SchedulerEnqueueJobResponse { job_id }))
}

async fn claim_job() -> Result<Json<SchedulerJobClaimResponse>, impl IntoResponse> {
    let mut queue = JOBS
        .get()
        .expect("Job queue not initialized")
        .lock()
        .unwrap();

    println!("want to claim a job");
    let Some(job) = queue.claim() else {
        return Err(StatusCode::NOT_FOUND);
    };

    Ok(Json(SchedulerJobClaimResponse {
        job_id: job.id().to_owned(),
        name: job.name().to_owned(),
        input: job.input().to_owned(),
    }))
}

async fn tickle_job(Path(_job_id): Path<Uuid>) -> impl IntoResponse {
    StatusCode::OK
}

async fn job_status(
    Path(job_id): Path<Uuid>,
    _state: State<AppState>,
) -> Result<Json<SchedulerJobStatusResponse>, impl IntoResponse> {
    let queue = JOBS
        .get()
        .expect("Job queue not initialized")
        .lock()
        .unwrap();

    let Some(job) = queue.get_job(job_id) else {
        return Err(StatusCode::NOT_FOUND);
    };

    Ok(Json(SchedulerJobStatusResponse {
        status: job.status().to_owned(),
        output: job.output().to_owned(),
    }))
}

async fn complete_job(
    Path(job_id): Path<Uuid>,
    output: Bytes,
) -> Result<impl IntoResponse, impl IntoResponse> {
    let mut queue = JOBS
        .get()
        .expect("Job queue not initialized")
        .lock()
        .unwrap();

    let Some(job) = queue.get_job_mut(job_id) else {
        return Err(StatusCode::NOT_FOUND);
    };

    log::info!(
        "Marking job {job_id} as complete with {} bytes of output",
        output.len()
    );

    match job.mark_complete(JobStatus::Completed, output.to_vec()) {
        Ok(_) => Ok(StatusCode::OK),
        Err(_) => Err(StatusCode::BAD_REQUEST),
    }
}
