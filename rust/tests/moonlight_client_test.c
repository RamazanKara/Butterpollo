#define main receiver_main
#include "moonlight_client.c"
#undef main
#include <assert.h>

static void test_picture(int format,int width,int height,int padded_width,int padded_height,int valid){
    requested_format=format;requested_width=width;requested_height=height;
    barcode_bottom=1;
    atomic_store(&failures,0);atomic_store(&decoded_frames,0);atomic_store(&detailed_frames,0);
    measured_frames=0;
    // Raw video exercises the receiver's decoded-picture path without a host.
    const AVCodec *codec=avcodec_find_decoder(AV_CODEC_ID_RAWVIDEO);
    decoder=avcodec_alloc_context3(codec);assert(decoder);
    decoder->width=padded_width;decoder->height=padded_height;decoder->pix_fmt=AV_PIX_FMT_GRAY8;
    assert(avcodec_open2(decoder,codec,NULL)==0);
    char *data=calloc((size_t)padded_width,padded_height);assert(data);
    data[(height/8)*padded_width+width/12]=(char)255;
    LARGE_INTEGER now;QueryPerformanceCounter(&now);
    uint32_t words[4]={123,(uint32_t)now.QuadPart,(uint32_t)((uint64_t)now.QuadPart>>32),0xB17E2212};
    for(int row=0;row<4;row++){
        int y=height-(128-20-row*24);
        data[y*padded_width+36]=(char)255;
        for(int bit=0;bit<32;bit++)if(words[row]&(1u<<bit))data[y*padded_width+72+bit*16]=(char)255;
    }
    LENTRY entry={.data=data,.length=padded_width*padded_height,.bufferType=BUFFER_TYPE_PICDATA};
    DECODE_UNIT unit={.frameNumber=1,.frameType=FRAME_TYPE_IDR,.fullLength=entry.length,.bufferList=&entry};
    assert(video_frame(&unit)==DR_OK);
    assert(atomic_load(&decoded_frames)==1);
    assert((atomic_load(&failures)==0)==valid);
    if(valid){
        assert(atomic_load(&detailed_frames)==1);
        assert(measured_frames==1&&picture_frames[0]==123&&picture_age[0]>=0);
    }
    avcodec_free_context(&decoder);free(data);
}

int main(void){
    test_picture(VIDEO_FORMAT_AV1_MAIN8,1920,1080,1920,1082,1);
    test_picture(VIDEO_FORMAT_AV1_MAIN8,1968,2184,1984,2186,1);
    test_picture(VIDEO_FORMAT_AV1_MAIN10,1920,1080,1984,1096,1);
    test_picture(VIDEO_FORMAT_AV1_MAIN8,1920,1080,1985,1080,0);
    test_picture(VIDEO_FORMAT_AV1_MAIN8,1920,1080,1920,1097,0);
    test_picture(VIDEO_FORMAT_AV1_MAIN8,1920,1080,1919,1080,0);
    test_picture(VIDEO_FORMAT_AV1_MAIN8,1920,1080,1920,1079,0);
    test_picture(VIDEO_FORMAT_H264,1920,1080,1920,1080,1);
    test_picture(VIDEO_FORMAT_H264,1920,1080,1920,1082,0);
    test_picture(VIDEO_FORMAT_H265,1920,1080,1920,1080,1);
    test_picture(VIDEO_FORMAT_H265,1920,1080,1984,1096,0);
    puts("Receiver picture tests passed");
    return 0;
}
