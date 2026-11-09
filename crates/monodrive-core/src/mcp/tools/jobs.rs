//! Following jobs: job_status, cancel_job and continue_job. File jobs and
//! saved commands' runs share them; a run's id starts with `cmd-`.

use super::Call;
use crate::mcp::contract::{Code, JobId, JobView, Reply, ToolError};
use crate::mcp::server::Server;
use crate::ops::{Conflict, Request};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const fn no_wait() -> u64 {
    0
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JobStatus {
    job: Option<String>,
    /// Hold the reply until a file job ends, up to 60 s.
    #[serde(default = "no_wait")]
    wait_seconds: u64,
}

#[derive(Serialize, JsonSchema)]
pub struct Jobs {
    pub jobs: Vec<JobView>,
}

impl Call for JobStatus {
    type Out = Jobs;

    fn run(self, server: &mut Server) -> Result<Reply<Jobs>, ToolError> {
        match self.job.as_deref().map(JobId::parse).transpose()? {
            Some(JobId::Job(id)) => {
                let reply = server.job_reply(id, self.wait_seconds)?;
                Ok(Reply::new(
                    reply.text,
                    Jobs {
                        jobs: vec![reply.data],
                    },
                ))
            }
            Some(JobId::Run(id)) => {
                let run = server
                    .ops
                    .runs()?
                    .into_iter()
                    .find(|run| run.id == id)
                    .ok_or_else(|| ToolError::new(Code::NotFound, format!("No job cmd-{id}.")))?;
                let view = JobView::of_run(&run);
                Ok(Reply::new(view.text(), Jobs { jobs: vec![view] }))
            }
            None => {
                let mut jobs: Vec<JobView> = server
                    .ops
                    .jobs()?
                    .iter()
                    .map(|job| JobView::of_job(job, |_| None))
                    .collect();
                jobs.extend(server.ops.runs()?.iter().map(JobView::of_run));
                let text: String = jobs.iter().map(|job| job.summary() + "\n").collect();
                Ok(Reply::new(text, Jobs { jobs }))
            }
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelJob {
    job: String,
}

#[derive(Serialize, JsonSchema)]
pub struct Cancelled {
    pub job: String,
}

impl Call for CancelJob {
    type Out = Cancelled;

    fn run(self, server: &mut Server) -> Result<Reply<Cancelled>, ToolError> {
        let text = match JobId::parse(&self.job)? {
            JobId::Job(job) => {
                server.submit(Request::Cancel { job }, None)?;
                format!("Cancelling job {job}; it keeps the items it finished.\n")
            }
            JobId::Run(run) => {
                server.ops.cancel_run(run)?;
                format!(
                    "Cancelling job {}. Commands aren't undoable: what it changed stays.\n",
                    self.job
                )
            }
        };
        Ok(Reply::new(text, Cancelled { job: self.job }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContinueJob {
    job: String,
    /// Replaces the job's own choice for the items left.
    conflict: Option<Conflict>,
    /// Hold the reply until the job ends, up to 60 s.
    #[serde(default = "no_wait")]
    wait_seconds: u64,
    request_id: Option<String>,
}

impl Call for ContinueJob {
    type Out = JobView;

    fn run(self, server: &mut Server) -> Result<Reply<JobView>, ToolError> {
        let JobId::Job(job) = JobId::parse(&self.job)? else {
            return Err(ToolError::new(
                Code::UnsupportedOnSource,
                "A command's run can't be continued; run_command starts it again.",
            ));
        };
        let request = Request::Retry {
            job,
            conflict: self.conflict,
        };
        // A continued job keeps its id.
        server.submit(request, self.request_id.as_deref())?;
        server.job_reply(job, self.wait_seconds)
    }
}
