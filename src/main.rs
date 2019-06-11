//! Stack-tail is a CLI for visualizing the state of AWS Cloudformation stacks
use chrono::{DateTime, FixedOffset};
use chrono_tz::Tz;
use colored::Colorize;
use console::Term;
use futures::{stream, Future, Stream};
use rusoto_cloudformation::{
    CloudFormation, CloudFormationClient, DescribeStackEventsError, DescribeStackEventsInput,
    DescribeStackResourcesError, DescribeStackResourcesInput, StackEvent, StackResource,
};
use rusoto_core::{credential::ChainProvider, request::HttpClient, Region, RusotoError};
use std::{error::Error as StdError, fmt, io::Write, thread::sleep, time::Duration};
use structopt::StructOpt;
use tabwriter::TabWriter;

const STACK_RESOURCE: &str = "AWS::CloudFormation::Stack";
const COMPLETE: &str = "_COMPLETE";
const FAILED: &str = "_FAILED";

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
        self.status.ends_with(COMPLETE) || self.status.ends_with(FAILED)
    }

    fn is_stack(&self) -> bool {
        self.resource_type == STACK_RESOURCE
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
            "{}\t{}\t{}\t{}\t{}",
            timestamp,
            state.resource_id.bold(),
            state.resource_type.bright_black(),
            match &state.status[..] {
                complete if complete.ends_with(COMPLETE) => format!(
                    "{} {}",
                    if complete.starts_with("DELETE") {
                        "⚰️ "
                    } else {
                        "✅"
                    },
                    state.status.bold().bright_green()
                ),
                failed if failed.ends_with(FAILED) => {
                    format!("❌ {}", state.status.bold().bright_red())
                }
                _ => format!("🔄 {}", state.status),
            },
            state.reason.bright_black()
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

#[derive(PartialEq)]
enum State {
    Init(bool),
    Next(bool, usize),
}

impl State {
    fn follow(&self) -> bool {
        match *self {
            State::Init(f) => f,
            State::Next(f, _) => f,
        }
    }

    fn complete(&self) -> bool {
        if let State::Next(false, _) = self {
            return true;
        }
        false
    }

    fn prev_len(&self) -> usize {
        match *self {
            State::Next(_, len) => len,
            _ => 0,
        }
    }
}

fn fetch_resources(
    cf: CloudFormationClient,
    stack_name: String,
    follow: bool,
) -> impl Stream<Item = (usize, Vec<ResourceState>), Error = Error> {
    stream::unfold(State::Init(follow), move |state| {
        if state.complete() {
            return None;
        }
        if let State::Next(_, _) = state {
            sleep(Duration::from_secs(1));
        }
        Some(
            cf.clone()
                .describe_stack_resources(DescribeStackResourcesInput {
                    stack_name: Some(stack_name.clone()),
                    ..DescribeStackResourcesInput::default()
                })
                .map(move |result| {
                    let states = result
                        .stack_resources
                        .unwrap_or_default()
                        .into_iter()
                        .map(ResourceState::from)
                        .collect::<Vec<_>>();
                    (
                        (state.prev_len(), states.clone()),
                        State::Next(
                            state.follow() && !states.iter().all(ResourceState::complete_or_failed),
                            states.len(),
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
) -> impl Stream<Item = (usize, Vec<ResourceState>), Error = Error> {
    stream::unfold(State::Init(follow), move |state| {
        if state.complete() {
            return None;
        }
        if let State::Next(_, _) = state {
            sleep(Duration::from_secs(1));
        }
        Some(
            cf.clone()
                .describe_stack_events(DescribeStackEventsInput {
                    stack_name: Some(stack_name.clone()),
                    ..DescribeStackEventsInput::default()
                })
                .map(move |result| {
                    let mut states = result
                        .stack_events
                        .unwrap_or_default()
                        .into_iter()
                        .map(ResourceState::from)
                        .collect::<Vec<_>>();
                    states.reverse();
                    (
                        (state.prev_len(), states.clone()),
                        State::Next(
                            state.follow()
                                && !states
                                    .last()
                                    .iter()
                                    .any(|state| state.is_stack() && state.complete_or_failed()),
                            states.len(),
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
) -> Box<dyn Stream<Item = (usize, Vec<ResourceState>), Error = Error> + Send + 'static> {
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

    let term = Term::stdout();
    let mut writer = TabWriter::new(term.clone());
    tokio::run(
        states(client(), stack_name, resources, follow)
            .for_each(move |result| {
                let (prev_len, states) = result;
                drop(term.clear_last_lines(prev_len));
                drop(writer.flush());
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
    fn state_communicates_followability() {
        for (state, expectation) in &[
            (State::Init(true), true),
            (State::Init(false), false),
            (State::Next(true, 0), true),
            (State::Next(false, 0), false),
        ] {
            assert_eq!(state.follow(), *expectation)
        }
    }

    #[test]
    fn state_is_complete_and_failure_aware() -> Result<(), chrono::format::ParseError> {
        for (status, expectation) in &[
            ("FOO_COMPLETE", true),
            ("FOO_FAILED", true),
            ("FOO_IN_PROGRESS", false),
        ] {
            assert_eq!(
                ResourceState {
                    resource_type: "foobar".into(),
                    timestamp: DateTime::parse_from_rfc3339("1996-12-19T16:39:57-08:00")?,
                    status: status.to_string(),
                    resource_id: "foobar".into(),
                    reason: "...".into(),
                }
                .complete_or_failed(),
                *expectation
            )
        }
        Ok(())
    }

    #[test]
    fn state_is_resource_aware() -> Result<(), chrono::format::ParseError> {
        for (resource_type, expectation) in &[(STACK_RESOURCE, true), ("not::a::stack", false)] {
            assert_eq!(
                ResourceState {
                    resource_type: resource_type.to_string(),
                    timestamp: DateTime::parse_from_rfc3339("1996-12-19T16:39:57-08:00")?,
                    status: "UPDATE_COMPLETE".into(),
                    resource_id: "foobar".into(),
                    reason: "...".into()
                }
                .is_stack(),
                *expectation
            )
        }
        Ok(())
    }

    #[test]
    fn state_tracks_prev_len() {
        assert_eq!(State::Next(false, 10).prev_len(), 10)
    }

    #[test]
    fn state_prev_len_for_init_is_zero() {
        assert_eq!(State::Init(false).prev_len(), 0)
    }

    #[test]
    fn state_is_complete_when_nothing_is_next() {
        assert!(State::Next(false, 0).complete())
    }

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
