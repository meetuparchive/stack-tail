//! Stack-tail is a CLI for visualizing the state of AWS Cloudformation stacks
use chrono::{DateTime, FixedOffset};
use chrono_tz::Tz;
use colored::Colorize;
use futures::{stream, Future, Stream};
use rusoto_cloudformation::{
    CloudFormation, CloudFormationClient, DescribeStackEventsError, DescribeStackEventsInput,
    DescribeStackResourcesError, DescribeStackResourcesInput, StackEvent, StackResource,
};
use rusoto_core::{credential::ChainProvider, request::HttpClient, Region, RusotoError};
use std::{fmt, io::Write, thread::sleep, time::Duration};
use structopt::StructOpt;
use tabwriter::TabWriter;

enum Error {
    Events(RusotoError<DescribeStackEventsError>),
    Resources(RusotoError<DescribeStackResourcesError>),
}

impl From<RusotoError<DescribeStackEventsError>> for Error {
    fn from(e: RusotoError<DescribeStackEventsError>) -> Self {
        Error::Events(e)
    }
}

impl From<RusotoError<DescribeStackResourcesError>> for Error {
    fn from(e: RusotoError<DescribeStackResourcesError>) -> Self {
        Error::Resources(e)
    }
}

#[derive(StructOpt, PartialEq, Debug)]
#[structopt(about = "Tails AWS CloudFormation events for a given stack")]
struct Options {
    #[structopt(
        short = "r",
        long = "resources",
        help = "Report summarized state for stack resources"
    )]
    resources: bool,
    #[structopt(
        short = "t",
        long = "timezone",
        help = "Display timestamps adjusted for the provided timezone.\nSee list of supported timezones here https://en.wikipedia.org/wiki/List_of_tz_database_time_zones#List"
    )]
    timezone: Option<Tz>,
    #[structopt(
        short = "f",
        long = "follow",
        help = "Follow the state of progress in changes to a stack until stack completion or failure"
    )]
    follow: bool,
    stack_name: String,
}

#[derive(Debug, Clone)]
struct ResourceState {
    resource_type: String,
    timestamp: DateTime<FixedOffset>,
    status: String,
    resource_id: String,
    reason: String,
}

impl ResourceState {
    fn complete_or_failed(&self) -> bool {
        self.status.ends_with("COMPLETE") || self.status.ends_with("FAILED")
    }

    fn is_stack(&self) -> bool {
        self.resource_type == "AWS::CloudFormation::Stack"
    }
}

/// Provides a means of displaying resource state
/// with time formatted for a given timezone
/// when provided
struct Formatted(ResourceState, Option<Tz>);

impl fmt::Display for Formatted {
    fn fmt(
        &self,
        f: &mut fmt::Formatter,
    ) -> fmt::Result {
        let Formatted(state, timezone) = self;
        let timestamp = match timezone {
            None => state.timestamp.to_string(),
            Some(tz) => state.timestamp.with_timezone(tz).to_string(),
        };
        write!(
            f,
            "{}\t{}\t{}\t{}",
            timestamp,
            state.resource_id.bold(),
            state.resource_type.bright_black(),
            match &state.status[..] {
                complete if complete.ends_with("_COMPLETE") => format!(
                    "{} {}",
                    if complete.starts_with("DELETE") {
                        "⚰️ "
                    } else {
                        "✅"
                    },
                    state.status.bold().bright_green()
                ),
                failed if failed.ends_with("_FAILED") => {
                    format!("❌ {}", state.status.bold().bright_red())
                }
                _ => format!("🔄 {}", state.status),
            }
        )
    }
}

impl From<StackEvent> for ResourceState {
    fn from(e: StackEvent) -> Self {
        ResourceState {
            resource_type: e.resource_type.unwrap_or_default(),
            timestamp: DateTime::parse_from_rfc3339(&e.timestamp).expect("invalid timestamp"),
            status: e.resource_status.unwrap_or_default(),
            resource_id: e.logical_resource_id.unwrap_or_default(),
            reason: e.resource_status_reason.unwrap_or_default(),
        }
    }
}

impl From<StackResource> for ResourceState {
    fn from(e: StackResource) -> Self {
        ResourceState {
            resource_type: e.resource_type,
            timestamp: DateTime::parse_from_rfc3339(&e.timestamp).expect("invalid timestamp"),
            status: e.resource_status,
            resource_id: e.logical_resource_id,
            reason: e.resource_status_reason.unwrap_or_default(),
        }
    }
}

enum FollowState {
    First(bool),
    Remaining(bool),
}

fn fetch_resources(
    cf: CloudFormationClient,
    stack_name: String,
    follow: bool,
) -> impl Stream<Item = Vec<ResourceState>, Error = Error> + Send + 'static {
    stream::unfold(FollowState::First(follow), move |state| {
        if let FollowState::Remaining(false) = state {
            return None;
        }
        Some(
            cf.clone()
                .describe_stack_resources(DescribeStackResourcesInput {
                    stack_name: Some(stack_name.clone()),
                    ..DescribeStackResourcesInput::default()
                })
                .map(move |result| {
                    let follow = match state {
                        FollowState::First(follow) => follow,
                        FollowState::Remaining(follow) => follow,
                    };
                    let states = result
                        .stack_resources
                        .unwrap_or_default()
                        .into_iter()
                        .map(ResourceState::from)
                        .collect::<Vec<_>>();
                    (
                        states.clone(),
                        FollowState::Remaining(
                            follow && !states.iter().all(ResourceState::complete_or_failed),
                        ),
                    )
                })
                .map_err(Error::from),
        )
    })
}

fn fetch_events(
    cf: CloudFormationClient,
    stack_name: String,
    follow: bool,
) -> impl Stream<Item = Vec<ResourceState>, Error = Error> + Send + 'static {
    stream::unfold(FollowState::First(follow), move |state| {
        if let FollowState::Remaining(false) = state {
            return None;
        }
        Some(
            cf.clone()
                .describe_stack_events(DescribeStackEventsInput {
                    stack_name: Some(stack_name.clone()),
                    ..DescribeStackEventsInput::default()
                })
                .map(move |result| {
                    let follow = match state {
                        FollowState::First(follow) => follow,
                        FollowState::Remaining(follow) => follow,
                    };
                    let states = result
                        .stack_events
                        .unwrap_or_default()
                        .into_iter()
                        .map(ResourceState::from)
                        .collect::<Vec<_>>();
                    (
                        states.clone(),
                        FollowState::Remaining(
                            follow
                                && !states
                                    .iter()
                                    .any(|state| state.is_stack() && state.complete_or_failed()),
                        ),
                    )
                })
                .map_err(Error::from),
        )
    })
}

/// Return a stream of cloud formation resoure states,
/// either for a aggregate list of resources for the resource
/// states over time
fn states(
    cf: CloudFormationClient,
    stack_name: String,
    resources: bool,
    follow: bool,
) -> Box<Stream<Item = Vec<ResourceState>, Error = Error> + Send + 'static> {
    if resources {
        Box::new(fetch_resources(cf, stack_name, follow))
    } else {
        Box::new(fetch_events(cf, stack_name, follow))
    }
}

fn credentials() -> ChainProvider {
    let mut chain = ChainProvider::new();
    chain.set_timeout(Duration::from_millis(200));
    chain
}

fn client() -> CloudFormationClient {
    CloudFormationClient::new_with(
        HttpClient::new().expect("failed to create request dispatcher"),
        credentials(),
        Region::default(),
    )
}

fn main() -> Result<(), Box<dyn StdError>> {
    let Options {
        stack_name,
        timezone,
        follow,
        resources,
    } = Options::from_args();

    let cf = client();
    let mut writer = TabWriter::new(std::io::stdout());
    tokio::run(
        states(cf, stack_name, resources, follow)
            .for_each(move |states| {
                for state in states {
                    drop(writeln!(&mut writer, "{}", Formatted(state, timezone)));
                }
                drop(writer.flush());
                Ok(())
            })
            .map_err(|_| ()),
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::America::New_York;

    #[test]
    fn options_require_stack_name() {
        assert!(Options::from_iter_safe(&["stack-tail"]).is_err())
    }

    #[test]
    fn options_parse_timezone() {
        assert_eq!(
            Options::from_iter(&["stack-tail", "-t", "America/New_York", "foo"]),
            Options {
                resources: false,
                timezone: Some(New_York),
                follow: false,
                stack_name: "foo".into(),
            }
        )
    }
}
