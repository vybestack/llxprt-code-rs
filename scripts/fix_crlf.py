# Convert the "literal CR" bytes out of tests/provider.rs.  The file was written with
# real CR bytes where the Rust source literally needs the two characters backslash r
# backslash n.  Only CR bytes that are followed by LF are the bogus ones (real line
# endings in this crate are LF only), so replace each 0x0D 0x0A with the two
# source characters '\' 'r' '\' 'n' and leave the LF.
p = 'tests/provider.rs'
s = open(p, 'rb').read()
out = s.replace(b'\r\n', b'\\r\\n')
open(p, 'wb').write(out)
print('converted CRLF->backslash-r-backslash-n, count was', s.count(b'\r\n'), 'remaining CR bytes:', out.count(b'\r'))
