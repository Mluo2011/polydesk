#ifndef POLYDESK_FFI_H
#define POLYDESK_FFI_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/// Start the in-process Rust PolyDesk CLOB V2 engine using `data_dir` (iOS Documents directory)
/// for local config and encrypted wallet vault storage. Returns 0 on success.
int32_t polydesk_start(const char *data_dir_utf8);

/// Execute a JSON command against the in-process Rust engine and return a heap-allocated
/// UTF-8 JSON response string. Caller MUST free the returned pointer via `polydesk_free_str`.
char *polydesk_call(const char *request_json_utf8);

/// Free a string previously returned by `polydesk_call`.
void polydesk_free_str(char *ptr);

#ifdef __cplusplus
}
#endif

#endif /* POLYDESK_FFI_H */
