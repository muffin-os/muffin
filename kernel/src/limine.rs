use limine::BaseRevision;
use limine::request::{
    DateAtBootRequest, ExecutableAddressRequest, ExecutableFileRequest, HhdmRequest,
    MemoryMapRequest, ModuleRequest, MpRequest, RequestsEndMarker, RequestsStartMarker,
    RsdpRequest, StackSizeRequest,
};

#[used]
#[unsafe(link_section = ".requests_start_marker")]
pub static _START_MARKER: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[unsafe(link_section = ".requests_end_marker")]
pub static _END_MARKER: RequestsEndMarker = RequestsEndMarker::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static BASE_REVISION: BaseRevision = BaseRevision::with_revision(3);

#[used]
#[unsafe(link_section = ".requests")]
pub static BOOT_TIME: DateAtBootRequest = DateAtBootRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static KERNEL_FILE_REQUEST: ExecutableFileRequest = ExecutableFileRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static KERNEL_ADDRESS_REQUEST: ExecutableAddressRequest = ExecutableAddressRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static MEMORY_MAP_REQUEST: MemoryMapRequest = MemoryMapRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static RSDP_REQUEST: RsdpRequest = RsdpRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static STACK_SIZE_REQUEST: StackSizeRequest = StackSizeRequest::new().with_size(262_144);

#[used]
#[unsafe(link_section = ".requests")]
pub static MODULE_REQUEST: ModuleRequest = ModuleRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static mut MP_REQUEST: MpRequest = MpRequest::new();
