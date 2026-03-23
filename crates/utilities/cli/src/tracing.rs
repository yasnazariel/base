//! Tracing subscriber initialization for CLI applications.

use std::{
    fmt,
    io,
    sync::{Arc, Once},
};

use tracing::Subscriber;
use tracing::level_filters::LevelFilter;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{
    EnvFilter, Layer,
    fmt::{
        FmtContext, FormattedFields,
        format::{FormatEvent, FormatFields, Writer},
        time::{FormatTime, SystemTime},
    },
    layer::SubscriberExt,
    registry::LookupSpan,
    reload,
    util::SubscriberInitExt,
};

use crate::{FileLogConfig, LogConfig, LogFormat, LogRotation, StdoutLogConfig};

type SetterFn = Arc<dyn Fn(&str) -> eyre::Result<()> + Send + Sync>;

/// A handle for changing the global log level at runtime.
///
/// Created by [`LogConfig::init_tracing_subscriber_with_reload`] and passed
/// into the admin server so that `admin_setLogLevel` works without restarting.
#[derive(Clone)]
pub struct LogReloadHandle {
    setter: SetterFn,
}

impl LogReloadHandle {
    /// Change the global log level to `level` (e.g. `"debug"`, `"info"`).
    pub fn set_level(&self, level: &str) -> eyre::Result<()> {
        (self.setter)(level)
    }

    /// Convert into a raw setter closure for use in crates that do not
    /// depend on `base-cli-utils`.
    pub fn into_setter(self) -> SetterFn {
        self.setter
    }
}

impl fmt::Debug for LogReloadHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LogReloadHandle").finish_non_exhaustive()
    }
}

/// Custom logfmt formatter for tracing events.
///
/// Outputs logs in logfmt format: `ts=... level=info target=myapp msg="hello world"`
#[derive(Debug, Clone, Copy, Default)]
pub struct LogfmtFormatter;

impl<S, N> FormatEvent<S, N> for LogfmtFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> fmt::Result {
        let meta = event.metadata();

        // Write timestamp
        let time_format = SystemTime;
        write!(writer, "time=\"")?;
        time_format.format_time(&mut writer)?;
        write!(writer, "\" ")?;

        // Write level
        write!(writer, "level={} ", meta.level())?;

        // Write target
        write!(writer, "target={} ", meta.target())?;

        // Write the message and fields
        write!(writer, "msg=\"")?;
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        write!(writer, "\"")?;

        // Write span context
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                write!(writer, " {}={{", span.name())?;
                if let Some(fields) = span.extensions().get::<FormattedFields<N>>() {
                    write!(writer, "{fields}")?;
                }
                write!(writer, "}}")?;
            }
        }

        writeln!(writer)
    }
}

impl LogConfig {
    /// Initialize the tracing subscriber with the configured options.
    ///
    /// This sets the global default subscriber. Should only be called once.
    ///
    /// Includes default noise suppression for overly verbose crates (e.g., `discv5=error`).
    pub fn init_tracing_subscriber(&self) -> eyre::Result<()> {
        // Build base filter from config, allowing env override
        let filter = EnvFilter::builder()
            .with_default_directive(self.global_level.into())
            .from_env_lossy()
            // Suppress noisy discovery logs by default
            .add_directive("discv5=error".parse().expect("valid directive"));

        self.init_tracing_subscriber_with_filter(filter)
    }

    /// Initialize the tracing subscriber and return a [`LogReloadHandle`] for
    /// runtime log level changes.
    ///
    /// This sets the global default subscriber. Should only be called once.
    /// Pass the returned handle to the admin server so that
    /// `admin_setLogLevel` can take effect without a process restart.
    pub fn init_tracing_subscriber_with_reload(&self) -> eyre::Result<LogReloadHandle> {
        let base_filter = EnvFilter::builder()
            .with_default_directive(self.global_level.into())
            .from_env_lossy()
            .add_directive("discv5=error".parse().expect("valid directive"));

        let (reload_filter, reload_handle) = reload::Layer::new(base_filter);

        let stdout_layer = self.stdout_logs.as_ref().map(build_stdout_layer);
        let file_layer = self.file_logs.as_ref().map(build_file_layer);

        tracing_subscriber::registry()
            .with(reload_filter)
            .with(stdout_layer)
            .with(file_layer)
            .try_init()
            .map_err(|e| eyre::eyre!("Failed to initialize tracing subscriber: {}", e))?;

        let setter = Arc::new(move |level: &str| -> eyre::Result<()> {
            let new_filter = EnvFilter::builder()
                .with_default_directive(
                    level
                        .parse::<LevelFilter>()
                        .map_err(|e| eyre::eyre!("invalid log level {:?}: {}", level, e))?
                        .into(),
                )
                .from_env_lossy()
                .add_directive("discv5=error".parse().expect("valid directive"));
            reload_handle
                .reload(new_filter)
                .map_err(|e| eyre::eyre!("reload failed: {}", e))
        });

        Ok(LogReloadHandle { setter })
    }

    /// Initialize the tracing subscriber with a custom filter.
    ///
    /// This sets the global default subscriber. Should only be called once.
    pub fn init_tracing_subscriber_with_filter(&self, filter: EnvFilter) -> eyre::Result<()> {
        let registry = tracing_subscriber::registry().with(filter);

        // Build stdout layer
        let stdout_layer = self.stdout_logs.as_ref().map(build_stdout_layer);

        // Build file layer
        let file_layer = self.file_logs.as_ref().map(build_file_layer);

        // Combine and init
        registry
            .with(stdout_layer)
            .with(file_layer)
            .try_init()
            .map_err(|e| eyre::eyre!("Failed to initialize tracing subscriber: {}", e))
    }
}

/// Build a stdout layer with the specified format.
fn build_stdout_layer<S>(config: &StdoutLogConfig) -> Box<dyn Layer<S> + Send + Sync>
where
    S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
{
    let base = tracing_subscriber::fmt::layer()
        .with_writer(io::stdout)
        .with_ansi(true)
        .with_timer(SystemTime);

    match config.format {
        LogFormat::Full => Box::new(base),
        LogFormat::Compact => Box::new(base.compact()),
        LogFormat::Json => Box::new(base.json()),
        LogFormat::Pretty => Box::new(base.pretty()),
        LogFormat::Logfmt => Box::new(base.event_format(LogfmtFormatter)),
    }
}

/// Build a file layer with the specified format and rotation.
fn build_file_layer<S>(config: &FileLogConfig) -> Box<dyn Layer<S> + Send + Sync>
where
    S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
{
    let rotation = match config.rotation {
        LogRotation::Minutely => Rotation::MINUTELY,
        LogRotation::Hourly => Rotation::HOURLY,
        LogRotation::Daily => Rotation::DAILY,
        LogRotation::Never => Rotation::NEVER,
    };

    let appender = RollingFileAppender::new(rotation, &config.directory_path, "base-node.log");

    let (non_blocking, guard) = tracing_appender::non_blocking(appender);

    // Store guard in a static to prevent dropping (leak intentionally to keep writer alive)
    std::mem::forget(guard);

    let base = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_timer(SystemTime);

    match config.format {
        LogFormat::Full => Box::new(base),
        LogFormat::Compact => Box::new(base.compact()),
        LogFormat::Json => Box::new(base.json()),
        LogFormat::Pretty => Box::new(base.pretty()),
        LogFormat::Logfmt => Box::new(base.event_format(LogfmtFormatter)),
    }
}

/// Initialize tracing for tests with sensible defaults.
///
/// Uses `tracing_subscriber::fmt().with_test_writer()` for test output capture.
/// Only initializes once (safe to call from multiple tests).
pub fn init_test_tracing() {
    static INIT: Once = Once::new();

    INIT.call_once(|| {
        let filter = EnvFilter::builder()
            .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
            .from_env_lossy();

        let _ = tracing_subscriber::fmt().with_env_filter(filter).with_test_writer().try_init();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_test_tracing_idempotent() {
        init_test_tracing();
        init_test_tracing(); // Should not panic
    }
}
