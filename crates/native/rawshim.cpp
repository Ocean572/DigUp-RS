// Issue #152: the C surface over the vendored LibRaw (`Vendor/libraw`, fetched by
// scripts/fetch-vendor.sh and built by scripts/build-libraw.sh). The names carry the `digup_`
// prefix like the stb renames — a static payload link must not mix engine and library copies.
//
// dcraw_process runs the camera's own pipeline and ROTATES for the known shot orientations, so
// the bitmap lands upright: the callers must NOT turn it again by the EXIF tag.
#include <libraw/libraw.h>
#include <cstdint>
#include <cstring>
#include <new>

extern "C" {

// Open + unpack, answer the full-size dimensions, and hand back the handle. Unpack is where a
// RAW file's own data lands in memory — the caller's budget guard runs between this and the
// render, exactly like the stb path's header-first preflight.
void* digup_raw_open(const char* path, int* width, int* height) {
    LibRaw* raw = new (std::nothrow) LibRaw();
    if (!raw) return nullptr;
    if (raw->open_file(path) != LIBRAW_SUCCESS) { delete raw; return nullptr; }
    if (raw->unpack() != LIBRAW_SUCCESS) { delete raw; return nullptr; }
    *width = raw->imgdata.sizes.iwidth;
    *height = raw->imgdata.sizes.iheight;
    return raw;
}

// The render: dcraw_process into 8-bit RGB, three bytes per pixel, row by row — the shape
// RGBImage carries. 0 answers success; anything else is "can't decode image" to the caller.
int digup_raw_render(void* handle, uint8_t* out, long long out_cap) {
    LibRaw* raw = static_cast<LibRaw*>(handle);
    raw->imgdata.params.output_tiff = 1;  // the 8-bit bitmap, not the JPEG encoder's output
    raw->imgdata.params.output_bps = 8;
    if (raw->dcraw_process() != LIBRAW_SUCCESS) return 1;
    libraw_processed_image_t* img = raw->dcraw_make_mem_image();
    if (!img) return 2;
    const long long need =
        (long long)img->width * (long long)img->height * (long long)img->colors;
    if (img->bits != 8 || img->colors != 3 || need > (long long)out_cap) {
        LibRaw::dcraw_clear_mem(img);
        return 3;
    }
    memcpy(out, img->data, (size_t)need);
    LibRaw::dcraw_clear_mem(img);
    return 0;
}

void digup_raw_close(void* handle) { delete static_cast<LibRaw*>(handle); }

}
