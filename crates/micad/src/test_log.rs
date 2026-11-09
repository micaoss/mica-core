//! A log capture for the tests that assert what never reaches the log.

use std::sync::{Arc, Mutex};

/// What `tracing` wrote while [`LogCapture::install`]'s guard lived.
#[derive(Clone, Default)]
pub struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl LogCapture {
    /// Capture this thread's events at every level until the guard drops.
    pub fn install(&self) -> tracing::subscriber::DefaultGuard {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(self.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .without_time()
            .finish();
        tracing::subscriber::set_default(subscriber)
    }

    /// The captured text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl std::io::Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl tracing_subscriber::fmt::MakeWriter<'_> for LogCapture {
    type Writer = Self;

    fn make_writer(&self) -> Self::Writer {
        self.clone()
    }
}

/// Assert that none of `markers` reached `captured`.
pub fn assert_quiet(captured: &str, markers: &[&str]) {
    for marker in markers {
        assert!(
            !captured.contains(marker),
            "a secret reached the log: {marker}\n{captured}"
        );
    }
}
