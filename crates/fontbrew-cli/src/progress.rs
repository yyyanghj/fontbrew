use fontbrew_core::{ProgressEvent, ProgressSink};

use crate::{
    exit::{CliError, CliResult},
    reporter::Reporter,
};

pub struct ProgressAdapter<'a> {
    reporter: &'a mut dyn Reporter,
    error: Option<CliError>,
}

impl<'a> ProgressAdapter<'a> {
    pub fn new(reporter: &'a mut dyn Reporter) -> Self {
        Self {
            reporter,
            error: None,
        }
    }

    pub fn finish(mut self) -> CliResult<()> {
        let activity_error = self.reporter.finish_activity().err();
        match self.error.take() {
            Some(error) => Err(error),
            None => match activity_error {
                Some(error) => Err(error),
                None => Ok(()),
            },
        }
    }
}

impl Drop for ProgressAdapter<'_> {
    fn drop(&mut self) {
        let _ = self.reporter.finish_activity();
    }
}

impl ProgressSink for ProgressAdapter<'_> {
    fn emit(&mut self, event: ProgressEvent) {
        if self.error.is_some() {
            return;
        }

        if let Err(error) = self.reporter.progress(&event) {
            self.error = Some(error);
        }
    }
}
