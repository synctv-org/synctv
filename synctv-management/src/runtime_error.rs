use std::borrow::Cow;

use synctv_adapter::error::{ClassifiedError, ErrorKind};

#[derive(Debug, Clone)]
pub struct RuntimeError {
    kind: ErrorKind,
    message: String,
}

impl RuntimeError {
    #[must_use]
    pub fn from_classified_error(error: &impl ClassifiedError) -> Self {
        Self {
            kind: error.classify(),
            message: error.message().into_owned(),
        }
    }
}

impl ClassifiedError for RuntimeError {
    fn classify(&self) -> ErrorKind {
        self.kind
    }

    fn message(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.message)
    }
}
