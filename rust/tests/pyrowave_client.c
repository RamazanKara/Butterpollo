// Independent vendor decoder and Moonlight wire framing probe. Never linked into the host.
#include <vulkan/vulkan.h>
#include <pyrowave/pyrowave.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
static uint32_t le(const unsigned char *p) { return p[0]|(p[1]<<8)|(p[2]<<16)|((uint32_t)p[3]<<24); }
int main(int argc,char **argv) {
    if (argc<2 || argc>3) return 2;
    FILE *f=fopen(argv[1],"rb"); if(!f)return 2;
    fseek(f,0,SEEK_END);long length=ftell(f);rewind(f);
    if(length<12 || length>64*1024*1024)return 2;
    unsigned char *data=malloc(length);if(!data || fread(data,1,length,f)!=(size_t)length)return 2;fclose(f);
    int record=(le(data)&0x80000000)!=0;
    const unsigned char *sequence=record?data:data+8;
    uint32_t word0=le(sequence),word1=le(sequence+4);
    int width=(word0&0x3fff)+1,height=((word0>>14)&0x3fff)+1,chroma=(word1>>26)&1;
    if(width>4096 || height>4096 || !(word0&0x80000000))return 3;
    pyrowave_device device=NULL;pyrowave_decoder decoder=NULL;
    if(pyrowave_create_device_by_compat(0,0,NULL,NULL,NULL,&device))return 4;
    pyrowave_decoder_create_info info={.device=device,.width=width,.height=height,.chroma=chroma,.fragment_path=false};
    if(pyrowave_decoder_create(&info,&decoder))return 5;
    size_t offset=record?0:4;unsigned count=record?0:le(data),pushed=0,dropped=0,seen=0;
    while(offset<(size_t)length) {
        size_t size; const unsigned char *packet;
        if(record) {
            if((size_t)length-offset<8)return 6;
            uint32_t h=le(data+offset);
            size=h==UINT32_MAX?8+(size_t)le(data+offset+4)*4:(h&0x80000000)?8:((h>>16)&0xfff)*4;
            packet=data+offset;
            if(size<8 || size>(size_t)length-offset)return 6;
            offset+=size;
            if(h==UINT32_MAX)continue;
            // Independently skip a small fraction of detail records. Coarse IDs
            // remain intact; the vendor sideband decoder must recover the frame.
            unsigned coarse=((width+1023)/1024)*((height+1023)/1024)*12;
            unsigned ordinal=seen++;
            if(argc==3 && !(h&0x80000000) && (le(packet+4)>>8)>=coarse && ordinal%40==0) { dropped++;continue; }
        } else {
            if((size_t)length-offset<4)return 6;
            size=le(data+offset);offset+=4;
            if(!size || size>(size_t)length-offset)return 6;
            packet=data+offset;offset+=size;
        }
        if(pyrowave_decoder_push_packet(decoder,packet,size))return 7;
        pushed++;
    }
    if(!record && pushed!=count)return 8;
    if(argc==3) { if(!record || !dropped || !pyrowave_decoder_decode_is_ready_with_sideband(decoder,true,0,0.9f,NULL,0))return 8; }
    else if(!pyrowave_decoder_decode_is_ready(decoder,false))return 8;
    size_t ybytes=(size_t)width*height,cbytes=ybytes/(chroma?1:4);
    unsigned char *pixels=calloc(1,ybytes+2*cbytes);
    pyrowave_cpu_buffer out={.data={pixels,pixels+ybytes,pixels+ybytes+cbytes},.row_stride_in_bytes={width,width/(chroma?1:2),width/(chroma?1:2)},.plane_size_in_bytes={ybytes,cbytes,cbytes},.width=width,.height=height,.format=chroma?PYROWAVE_CPU_BUFFER_FORMAT_YUV444P:PYROWAVE_CPU_BUFFER_FORMAT_YUV420P};
    if(pyrowave_decoder_decode_cpu_buffer_synchronous(decoder,&out))return 9;
    unsigned long long total=0;for(size_t i=0;i<ybytes;i++)total+=pixels[i];
    if(!total)return 10;
    printf("PYROWAVE DECODE PASS framing=%s width=%d height=%d chroma=%s records=%u lost=%u mean_luma=%.3f\n",record?"records":"lengths",width,height,chroma?"444":"420",pushed,dropped,(double)total/ybytes);
    pyrowave_decoder_destroy(decoder);pyrowave_device_destroy(device);free(pixels);free(data);return 0;
}
