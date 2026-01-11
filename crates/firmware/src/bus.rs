/// Legacy firmware-side bus surface used by some platform integration tests.
///
/// The main BIOS implementation (`firmware::bios`) has a more structured memory bus
/// interface (`machine::MemoryAccess`, etc). These helpers exist to preserve older
/// unit/integration tests while the firmware crate converges on a single API.
pub trait Bus {
    fn read_u8(&mut self, paddr: u32) -> u8;
    fn write_u8(&mut self, paddr: u32, val: u8);

    fn a20_enabled(&self) -> bool;
    fn set_a20_enabled(&mut self, enabled: bool);

    fn io_read_u8(&mut self, port: u16) -> u8;
    fn io_write_u8(&mut self, port: u16, val: u8);

    fn serial_write(&mut self, byte: u8);
}

