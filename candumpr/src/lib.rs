pub mod can;
pub mod errframe;
pub mod format;
pub mod frame;
pub mod pipeline;
pub mod recv;
pub mod sink;
pub mod writer;

#[cfg(test)]
pub(crate) mod test_util {
    pub(crate) struct TestBufWriter {
        pub(crate) bytes: Vec<u8>,
    }

    impl TestBufWriter {
        pub(crate) fn new() -> Self {
            Self { bytes: Vec::new() }
        }
    }

    impl crate::writer::Writer for TestBufWriter {
        fn write(&mut self, b: &[u8]) -> eyre::Result<()> {
            self.bytes.extend_from_slice(b);
            Ok(())
        }

        fn flush(&mut self) -> eyre::Result<()> {
            Ok(())
        }

        fn sync(&mut self) -> eyre::Result<()> {
            Ok(())
        }

        fn finish(&mut self) -> eyre::Result<()> {
            Ok(())
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }
}

#[cfg(test)]
#[ctor::ctor]
fn test_setup() {
    tracing_subscriber::fmt().with_test_writer().init();
    vcan_fixture::enter_namespace();
}
