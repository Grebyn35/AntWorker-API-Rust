//! Where `server-setup` asks its questions: the terminal.

use std::io::{self, BufRead, IsTerminal, Write};

use super::questions::Prompt;

/// The terminal: questions on stdout and answers from stdin, and a secret typed unseen. Without
/// a terminal, such as when a script answers, each answer is a line of stdin.
pub(super) struct Terminal;

impl Prompt for Terminal {
    #[expect(clippy::print_stdout, reason = "the questions are the task's output")]
    fn show(&mut self, text: &str) {
        println!("{text}");
    }

    #[expect(clippy::print_stdout, reason = "the questions are the task's output")]
    fn ask(&mut self, question: &str) -> Result<String, String> {
        print!("{question}");
        io::stdout().flush().map_err(|error| error.to_string())?;
        let mut answer = String::new();
        let read = io::stdin()
            .lock()
            .read_line(&mut answer)
            .map_err(|error| format!("can't read the answer: {error}"))?;
        if read == 0 {
            return Err("the answers ended before the questions".to_owned());
        }
        Ok(answer.trim().to_owned())
    }

    fn ask_secret(&mut self, question: &str) -> Result<String, String> {
        if !io::stdin().is_terminal() {
            return self.ask(question);
        }
        rpassword::prompt_password(question)
            .map(|answer| answer.trim().to_owned())
            .map_err(|error| format!("can't read the answer: {error}"))
    }
}
