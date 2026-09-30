// Independent vendor decoder probe; this C test is never linked into the host.
#include <vulkan/vulkan.h>
#include <pyrowave/pyrowave.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    FILE *f = fopen(argv[1], "rb"); if (!f) return 2;
    fseek(f, 0, SEEK_END); long length = ftell(f); rewind(f);
    if (length < 12 || length > 64*1024*1024) return 2;
    unsigned char *data = malloc(length); if (!data || fread(data,1,length,f) != (size_t)length) return 2; fclose(f);
    if (memcmp(data,"PYRW\1",5)) return 3;
    unsigned count = (data[5]<<8) | data[6]; size_t offset = 8;
    pyrowave_device device = NULL; pyrowave_decoder decoder = NULL;
    if (pyrowave_create_device_by_compat(0,0,NULL,NULL,NULL,&device)) return 4;
    pyrowave_decoder_create_info info = { .device=device, .width=256, .height=256, .chroma=PYROWAVE_CHROMA_SUBSAMPLING_420, .fragment_path=false };
    if (pyrowave_decoder_create(&info,&decoder)) return 5;
    for (unsigned i=0;i<count;i++) {
        if (offset + 4 > (size_t)length) return 6;
        size_t size = (data[offset]<<24) | (data[offset+1]<<16) | (data[offset+2]<<8) | data[offset+3]; offset += 4;
        if (!size || size > (size_t)length-offset) return 6;
        if (pyrowave_decoder_push_packet(decoder,data+offset,size)) return 7;
        offset += size;
    }
    if (offset != (size_t)length || !pyrowave_decoder_decode_is_ready(decoder,false)) return 8;
    unsigned char *pixels = calloc(1,256*256*3);
    pyrowave_cpu_buffer out = { .data={pixels,pixels+256*256,pixels+256*256+128*128}, .row_stride_in_bytes={256,128,128}, .plane_size_in_bytes={256*256,128*128,128*128}, .width=256, .height=256, .format=PYROWAVE_CPU_BUFFER_FORMAT_YUV420P };
    if (pyrowave_decoder_decode_cpu_buffer_synchronous(decoder,&out)) return 9;
    unsigned total=0; for (size_t i=0;i<256*256;i++) total += pixels[i];
    if (!total) return 10;
    printf("PYROWAVE DECODE PASS packets=%u hdr=%u mean_luma=%.3f\n",count,data[7],(double)total/(256*256));
    pyrowave_decoder_destroy(decoder); pyrowave_device_destroy(device); free(pixels); free(data); return 0;
}
