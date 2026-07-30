pub mod can;
pub mod debounce;
pub mod errframe;
pub mod format;
pub mod frame;
pub mod pipeline;
pub mod recv;
pub mod sink;
pub mod writer;

#[cfg(test)]
#[ctor::ctor]
fn test_setup() {
    tracing_subscriber::fmt().with_test_writer().init();
    vcan_fixture::enter_namespace();
}
