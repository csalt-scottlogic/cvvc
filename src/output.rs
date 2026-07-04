/// A convenience type representing a function that takes an [`OutputMessage`] argument.
pub type Printer = dyn Fn(&OutputMessage);

/// Whether to produce plain or coloured output
///
/// "Plain" or "Colour" here should be taken as meaning
/// "output the `plain` property of the [`OutputMessage`] struct", and similarly,
/// "Colour" means "output the `colour` property if it is set".  By convention,
/// the calling code is expected to populate the properties directly.
pub enum OutputKind {
    /// When outputting, output the `OutputMessage.plain` member.
    Plain,

    /// When outputting, output the `OutputMessage.colour` member if it is `Some`.
    Colour,
}

/// An output message.
///
/// The message consists of a `plain` member which is expected to contain plain text,
/// and a `colour` member which is expected to contain text decorated with control codes
/// to specific colouring.
pub struct OutputMessage<'a> {
    plain: &'a str,
    colour: Option<&'a str>,
}

impl<'a> OutputMessage<'a> {
    /// Create an [`OutputMessage`], consisting of a plain message and an optional coloured message.
    pub fn new(plain: &'a str, colour: Option<&'a str>) -> OutputMessage<'a> {
        Self { plain, colour }
    }
}

/// A service that can print user messages.
pub trait OutputService {
    /// Print a user message, followed by a newline.
    fn println(&self, msg: &OutputMessage);
}

/// A service that prints user messages to the console.
pub struct ConsoleOutputService {
    mode: OutputKind,
}

impl ConsoleOutputService {
    /// Create a new [`ConsoleOutputService`], specifying whether it supports plain or coloured (and plain) messages.
    pub fn new(kind: OutputKind) -> Self {
        Self { mode: kind }
    }

    fn select<'a>(&self, msg: &'a OutputMessage) -> &'a str {
        match self.mode {
            OutputKind::Plain => msg.plain,
            OutputKind::Colour => msg.colour.unwrap_or(msg.plain),
        }
    }
}

impl OutputService for ConsoleOutputService {
    fn println(&self, msg: &OutputMessage) {
        println!("{}", self.select(msg))
    }
}

#[cfg(test)]
mod tests {
    use super::{ConsoleOutputService, OutputKind, OutputMessage};

    #[test]
    fn console_output_service_select_returns_plain_if_mode_is_plain_and_colour_is_set() {
        let test_input = OutputMessage {
            plain: "the expected message",
            colour: Some("fail"),
        };
        let test_object = ConsoleOutputService {
            mode: OutputKind::Plain,
        };

        let test_output = test_object.select(&test_input);

        assert_eq!("the expected message", test_output);
    }

    #[test]
    fn console_output_service_select_returns_plain_if_mode_is_plain_and_colour_is_not_set() {
        let test_input = OutputMessage {
            plain: "the expected message",
            colour: None,
        };
        let test_object = ConsoleOutputService {
            mode: OutputKind::Plain,
        };

        let test_output = test_object.select(&test_input);

        assert_eq!("the expected message", test_output);
    }

    #[test]
    fn console_output_service_select_returns_colour_if_mode_is_colour_and_colour_is_set() {
        let test_input = OutputMessage {
            plain: "fail",
            colour: Some("the expected message"),
        };
        let test_object = ConsoleOutputService {
            mode: OutputKind::Colour,
        };

        let test_output = test_object.select(&test_input);

        assert_eq!("the expected message", test_output);
    }

    #[test]
    fn console_output_service_select_returns_plain_if_mode_is_colour_and_colour_is_not_set() {
        let test_input = OutputMessage {
            plain: "the expected message",
            colour: None,
        };
        let test_object = ConsoleOutputService {
            mode: OutputKind::Colour,
        };

        let test_output = test_object.select(&test_input);

        assert_eq!("the expected message", test_output);
    }
}
