#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum State {
    Ready,
    Running,
    Finished,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ShouldTerminate {
    No,
    Yes,
}

impl ShouldTerminate {
    #[must_use]
    pub fn yes(self) -> bool {
        matches!(self, Self::Yes)
    }
}
