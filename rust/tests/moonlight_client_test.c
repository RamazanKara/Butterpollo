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
    uint64_t received_us=(uint64_t)(clock_ms()*1000);
    DECODE_UNIT unit={.frameNumber=1,.frameType=FRAME_TYPE_IDR,.frameHostProcessingLatency=22,
        .receiveTimeUs=received_us,.enqueueTimeUs=received_us,.presentationTimeUs=12345,
        .fullLength=entry.length,.bufferList=&entry};
    assert(video_frame(&unit)==DR_OK);
    assert(atomic_load(&decoded_frames)==1);
    assert((atomic_load(&failures)==0)==valid);
    if(valid){
        assert(atomic_load(&detailed_frames)==1);
        assert(measured_frames==1&&picture_frames[0]==123&&picture_age[0]>=0);
    }
    avcodec_free_context(&decoder);free(data);
}

static void test_audio(void){
    unsigned char mapping[2]={0,1},packet[4096];int error;
    OpusMSEncoder *encoder=opus_multistream_encoder_create(48000,2,1,1,mapping,OPUS_APPLICATION_AUDIO,&error);
    assert(encoder&&error==OPUS_OK);
    opus_decoder=opus_multistream_decoder_create(48000,2,1,1,mapping,&error);
    assert(opus_decoder&&error==OPUS_OK);
    audio_channels=2;audio_samples=48000*2*audio_channels;
    float samples[480]={0};
    int size=opus_multistream_encode_float(encoder,samples,240,packet,sizeof(packet));assert(size>0);
    atomic_store(&failures,0);
    audio_frame((char*)packet,size);
    assert(atomic_load(&failures)==0&&atomic_load(&audio_packets)==1&&audio_tone_blocks==2);
    opus_multistream_encoder_destroy(encoder);opus_multistream_decoder_destroy(opus_decoder);
    opus_decoder=NULL;
}

static void test_measurements(void){
    fclose(timing_csv);timing_csv=NULL;fclose(audio_csv);audio_csv=NULL;
    char line[1024];FILE *file=fopen("test-video.csv","r");assert(file);
    assert(fgets(line,sizeof(line),file));
    assert(strcmp(line,"wire_frame,arrival_ms,host_ms,decode_ms,render_frame,render_qpc,picture_age_ms,first_packet_us,assembled_us,presentation_us,frame_type\n")==0);
    assert(fgets(line,sizeof(line),file));
    int wire,type;unsigned sequence;unsigned long long ticks,received,assembled,presented;
    double arrival,host,decode,age;
    assert(sscanf(line,"%d,%lf,%lf,%lf,%u,%llu,%lf,%llu,%llu,%llu,%d",
        &wire,&arrival,&host,&decode,&sequence,&ticks,&age,&received,&assembled,&presented,&type)==11);
    assert(wire==1&&arrival>0&&host==2.2&&decode>=0&&sequence==123&&ticks>0&&age>=0);
    assert(received>0&&assembled==received&&presented==12345&&type==FRAME_TYPE_IDR);
    int rows=1;while(fgets(line,sizeof(line),file))rows++;
    assert(rows==11);fclose(file);
    file=fopen("test-audio.csv","r");assert(file);
    assert(fgets(line,sizeof(line),file));
    assert(strcmp(line,"arrival_ms,samples,min_rms,max_rms\n")==0);
    assert(fgets(line,sizeof(line),file));
    int samples;double minimum,maximum;
    assert(sscanf(line,"%lf,%d,%lf,%lf",&arrival,&samples,&minimum,&maximum)==4);
    assert(arrival>0&&samples==240&&minimum>=0&&maximum>=minimum);
    assert(!fgets(line,sizeof(line),file));fclose(file);
}

int main(void){
    _putenv_s("BUTTERPOLLO_TEST_TIMING_CSV","test-video.csv");
    _putenv_s("BUTTERPOLLO_TEST_AUDIO_CSV","test-audio.csv");
    _putenv_s("BUTTERPOLLO_TEST_AUDIO_TONE","1");
    _putenv_s("BUTTERPOLLO_TEST_HOST","127.0.0.1");
    // Invalid geometry opens the real CSV outputs, then exits before any
    // connection. The callbacks below feed them entirely offline.
    char *args[]={"moonlight-client","unused","hevc","1"};
    assert(receiver_main(4,args)==2);
    assert(timing_csv&&audio_csv);
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
    test_audio();test_measurements();
    puts("Receiver picture and time-series tests passed");
    return 0;
}
