use kernel_abi::ProcessId;

pub trait ProcessesAccess {
    type Process: ProcessAccess;

    fn all_processes(&self) -> impl Iterator<Item = Self::Process>;

    fn process_by_id(&self, pid: ProcessId) -> Option<Self::Process> {
        self.all_processes().find(|p| p.process_id() == pid)
    }

    fn processes_in_group(
        &self,
        process_group_id: ProcessId,
    ) -> impl Iterator<Item = Self::Process> {
        self.all_processes()
            .filter(move |p| p.process_group_id() == process_group_id)
    }
}

pub trait ProcessAccess {
    fn process_id(&self) -> ProcessId;
    fn process_group_id(&self) -> ProcessId;
}
