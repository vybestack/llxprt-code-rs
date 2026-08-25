use super::*;

struct PartialThenError {
    returned_data: bool,
}

impl std::io::Read for PartialThenError {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.returned_data {
            return Err(std::io::Error::other("injected read failure"));
        }
        self.returned_data = true;
        let data = b"partial match\n";
        buffer[..data.len()].copy_from_slice(data);
        Ok(data.len())
    }
}

#[test]
fn partial_search_read_failure_is_an_error() {
    let error = read_search_source(
        PartialThenError {
            returned_data: false,
        },
        1024,
        "nested/file.txt",
    )
    .expect_err("partial bytes must not turn a later read failure into success");
    assert!(error.contains("read nested/file.txt"), "{error}");
    assert!(error.contains("injected read failure"), "{error}");
}
