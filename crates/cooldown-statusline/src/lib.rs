//! Cooldown's Claude Code statusLine hook.
//!
//! Claude Code pipes a JSON document to the configured `statusLine` command on
//! every update. That document carries `rate_limits.five_hour` and
//! `rate_limits.seven_day` for Pro/Max subscribers, which is the official,
//! terms-of-service-safe source of subscription limits. This crate:
//!
//! * captures those fields into a small, versioned inbox file for the Cooldown
//!   app ([`hook`], [`parse`], [`inbox`]),
//! * keeps the user's previous statusline working by chaining to it
//!   ([`chain`]),
//! * installs and uninstalls itself in the Claude settings file without
//!   clobbering anything ([`install`], [`command`]).
//!
//! Everything that depends on the platform (paths, quoting, shell choice) is a
//! pure function that takes its inputs as arguments, so the Windows behaviour is
//! unit-tested on any machine.

pub mod antigravity;
pub mod atomic;
pub mod chain;
pub mod cli;
pub mod command;
pub mod hook;
pub mod inbox;
pub mod install;
pub mod parse;
pub mod paths;
