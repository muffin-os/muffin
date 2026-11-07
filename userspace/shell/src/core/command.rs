/// One input line of the shell.
#[derive(Debug, PartialEq, Eq)]
pub enum Command<'a> {
    /// The line holds only blanks.
    Empty,
    /// `echo`: the text after `echo `, verbatim.
    Echo(&'a str),
    /// `cd`: the directory to change to. `None` means the root.
    Cd(Option<&'a str>),
    /// `ls`: the directory to list. `None` means the current directory.
    Ls(Option<&'a str>),
    /// `tree`: the directory to show with everything below it. `None` means
    /// the current directory.
    Tree(Option<&'a str>),
    /// `poweroff`: stop the machine. Arguments are ignored.
    Poweroff,
    /// Any other command name.
    Unknown(&'a str),
}

impl<'a> Command<'a> {
    /// Splits `line` at the first blank after the command name.
    #[must_use]
    pub fn parse(line: &'a str) -> Self {
        let line = line.trim_start();
        if line.is_empty() {
            return Self::Empty;
        }
        let (name, rest) = line.split_once(' ').unwrap_or((line, ""));
        let path = Some(rest.trim()).filter(|path| !path.is_empty());
        match name {
            "echo" => Self::Echo(rest),
            "cd" => Self::Cd(path),
            "ls" => Self::Ls(path),
            "tree" => Self::Tree(path),
            "poweroff" => Self::Poweroff,
            _ => Self::Unknown(name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_echo_when_parsed_then_keeps_rest_verbatim() {
        assert_eq!(Command::parse("echo  a b "), Command::Echo(" a b "));
        assert_eq!(Command::parse("echo"), Command::Echo(""));
    }

    #[test]
    fn given_path_commands_when_parsed_then_trims_path() {
        assert_eq!(Command::parse(" cd  /etc "), Command::Cd(Some("/etc")));
        assert_eq!(Command::parse("ls"), Command::Ls(None));
        assert_eq!(Command::parse("ls .."), Command::Ls(Some("..")));
        assert_eq!(Command::parse("tree /var "), Command::Tree(Some("/var")));
    }

    #[test]
    fn given_poweroff_with_arguments_when_parsed_then_poweroff() {
        assert_eq!(Command::parse("poweroff now"), Command::Poweroff);
    }

    #[test]
    fn given_other_lines_when_parsed_then_empty_or_unknown() {
        assert_eq!(Command::parse("   "), Command::Empty);
        assert_eq!(Command::parse("foo bar"), Command::Unknown("foo"));
    }
}
