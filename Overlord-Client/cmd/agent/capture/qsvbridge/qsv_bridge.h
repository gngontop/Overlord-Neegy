// Keep this filename distinct from amfbridge/bridge_windows.h: cgo resolves
// the bare quoted include against the wrong package header when both use the
// same basename, producing "could not determine what C.overlord_qsv_* refers to".
#pragma once
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif
typedef void* overlord_qsv_encoder;
int overlord_qsv_probe(char* error_text, int error_capacity);
int overlord_qsv_create(void* d3d11_device, int input_width, int input_height,
                        int encode_width, int encode_height, int fps,
                        uint32_t dxgi_format, int bitrate,
                        overlord_qsv_encoder* encoder,
                        char* error_text, int error_capacity);
int overlord_qsv_encode(overlord_qsv_encoder encoder, void* d3d11_texture,
                        int force_idr, uint8_t* output, int output_capacity,
                        int* output_size, char* error_text, int error_capacity);
void overlord_qsv_destroy(overlord_qsv_encoder encoder);
#ifdef __cplusplus
}
#endif
