# P6 independent oracle

`v02_expected.json` is hand-authored from the frozen W1–W4 requirements and
the C/D/H/S modification contracts.  It is not generated from Mimi output.
`v02_ffi.c` is the trusted host reference for the scalar checksum ABI; the
runner builds it into a temporary shared library for both the VM and native
checks.  Event order for the concurrent queue is compared as an allowed
permutation, while the aggregate result remains fixed.
