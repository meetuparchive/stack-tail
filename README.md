# stack tail [![Build Status](https://travis-ci.com/meetup/stack-tail.svg?token=jtveWukBghqdyHppHDFu&branch=master)](https://travis-ci.com/meetup/stack-tail) [![Coverage Status](https://coveralls.io/repos/github/meetup/stack-tail/badge.svg)](https://coveralls.io/github/meetup/stack-tail)

> 🥞 ☁️ A CLI interface for monitoring the state and progress of [AWS CloudFormation](https://aws.amazon.com/cloudformation/) stacks

## 🤔 about

Despite leveraging virtuous a continuously deployment model for your application and infrastructure
you may find yourself curious about the current state of your Cloud Formation stack. Visiting 
the AWS console can pull you out of your _flow_ using the aws cli can put a strain on your eyes. 
`stack-tail` is meant to fill the cap and draw a quick and clear picture to understanding the state
of your stack.

## 📦 install

Via github releases
Prebuilt binaries for osx and linux are available for download directly from Github Releases

```sh
$ curl -L \
 "https://github.com/meetup/stack-tail/releases/download/v0.0.0/stack-tail-v0.0.0-$(uname -s)-$(uname -m).tar.gz" \
  | tar -xz
```

## 🤸 usage

This tool communicates with AWS Cloud Formation APIs using the standard [AWS credential chain](https://docs.aws.amazon.com/cli/latest/userguide/cli-chap-configure.html)
to authenticate requests. You may wish to export an `AWS_PROFILE` env variable to query your stacks from different accounts or different regions.

The main use case for this CLI quickly assessing the state of a target CloudFormation stack by tailing its active or current state.

> 💡You can get of available list of stack names with the following AWS cli command
> ```sh
> $ aws cloudformation list-stacks \
>    --query 'StackSummaries[*].StackName' \
>    --output=json
> ```

```sh
USAGE:
    stack-tail [FLAGS] [OPTIONS] <stack_name>

FLAGS:
    -f, --follow       Follow the state of progress in changes to a stack until stack completion or failure
    -h, --help         Prints help information
    -r, --resources    Report summarized state for stack resources
    -V, --version      Prints version information

OPTIONS:
    -t, --timezone <timezone>    Display timestamps adjusted for the provided timezone.
                                 See list of supported timezones here
                                 https://en.wikipedia.org/wiki/List_of_tz_database_time_zones#List

ARGS:
    <stack_name>
```

### events

The default view is a list of stack update events

```sh
$ stack-tail my-stack-name
```

## resources

In some cases you may wish to only want to get a picture of the aggregate list of stack resources.
You can use the `--resources` or `-r` flag to get this insight

```sh
$ stack-tail -r my-stack-name
```


## 👩‍🏭 development

This is a [rustlang](https://www.rust-lang.org/en-US/) application.
Go grab yourself a copy with [rustup](https://rustup.rs/).

Meetup Inc 2019