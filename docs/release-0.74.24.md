# SeraBot 0.74.24

## Highlights

- **Windows CI Test Reliability Fix (`session.rs`)**: Hardened `session::backlog_tests::failed_providers_load_sets_section_load_error` for `windows-latest` by flushing and shutting down the mock TCP write half (`Shutdown::Write`) to avoid Windows loopback `RST` races, using an `AtomicBool` stop flag instead of a fixed 2-accept loop, and raising the completion deadline from 5 s to 15 s.
