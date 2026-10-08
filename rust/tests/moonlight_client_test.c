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

static void test_d3d11_readback(void){
    AVBufferRef *device=NULL;
    if(av_hwdevice_ctx_create(&device,AV_HWDEVICE_TYPE_D3D11VA,NULL,NULL,0)<0){
        puts("D3D11 barcode readback tests skipped: no D3D11 device");return;
    }
    const struct {int bottom;double scale,left,margin;int strip_left,width,rows[4],valid;} cases[]={
        {0,1,0,0,0,640,{20,44,68,92},1},
        {1,1,0,0,0,640,{2076,2100,2124,2148},1},
        {0,.625,13.5,7.25,12,402,{12,27,42,57},1},
        {1,.625,13.5,7.25,12,402,{2110,2125,2140,2155},1},
        {0,.5,-5,0,0,316,{10,22,34,46},1},
        {1,1,1345.5,0,1344,624,{2076,2100,2124,2148},1},
        {0,1,1600,0,1600,368,{20,44,68,92},0},
        {1,1,0,2200,0,640,{-124,-100,-76,-52},0},
        {1,1,0,-200,0,640,{2276,2300,2324,2348},0}
    };
    requested_format=VIDEO_FORMAT_AV1_MAIN10;requested_width=1968;requested_height=2184;
    for(int ten_bit=0;ten_bit<2;ten_bit++){
        AVBufferRef *pool=av_hwframe_ctx_alloc(device);assert(pool);
        AVHWFramesContext *context=(AVHWFramesContext*)pool->data;
        context->format=AV_PIX_FMT_D3D11;context->sw_format=ten_bit?AV_PIX_FMT_P010:AV_PIX_FMT_NV12;
        context->width=1984;context->height=2200;context->initial_pool_size=2;
        assert(av_hwframe_ctx_init(pool)==0);
        AVFrame *decoded=av_frame_alloc(),*other=av_frame_alloc(),*cpu=av_frame_alloc(),*frame=av_frame_alloc();
        assert(decoded&&other&&cpu&&frame);
        assert(av_hwframe_get_buffer(pool,decoded,0)==0&&av_hwframe_get_buffer(pool,other,0)==0);
        if(!decoded->data[1]){AVFrame *swap=decoded;decoded=other;other=swap;}
        assert(decoded->data[0]==other->data[0]&&(uintptr_t)decoded->data[1]==1&&!other->data[1]);
        cpu->format=context->sw_format;cpu->width=context->width;cpu->height=context->height;
        assert(av_frame_get_buffer(cpu,32)==0);
        memset(cpu->data[0],0,(size_t)cpu->linesize[0]*cpu->height);
        memset(cpu->data[1],0,(size_t)cpu->linesize[1]*cpu->height/2);
        assert(av_hwframe_transfer_data(other,cpu,0)==0);
        decoded->color_primaries=AVCOL_PRI_BT2020;decoded->color_trc=AVCOL_TRC_SMPTE2084;
        for(unsigned i=0;i<sizeof(cases)/sizeof(cases[0]);i++){
            barcode_bottom=cases[i].bottom;barcode_scale=cases[i].scale;
            barcode_left=cases[i].left;barcode_bottom_margin=cases[i].margin;
            memset(cpu->data[0],0,(size_t)cpu->linesize[0]*cpu->height);
            uint32_t words[4]={123+i,0x12345678,0x9abcdef0,0xB17E2212};
            for(int row=0;row<4;row++)for(int column=0;column<34;column++){
                int x=(int)(barcode_left+(column==0?12:column==1?36:72+(column-2)*16)*barcode_scale);
                int y=cases[i].rows[row];
                if(x<0||x>=requested_width||y<0||y>=requested_height)continue;
                int white=column==1||(column>=2&&(words[row]&(1u<<(column-2))));
                unsigned char *sample=cpu->data[0]+(size_t)y*cpu->linesize[0]+x*(ten_bit?2:1);
                unsigned value=white?(ten_bit?1023u<<6:255):0;
                sample[0]=value;if(ten_bit)sample[1]=value>>8;
            }
            assert(av_hwframe_transfer_data(decoded,cpu,0)==0);
            AVFrame strip={0};int left=-1;
            assert(readback_barcode(decoded,frame,&strip,&left)==0);
            assert(frame->width==1968&&frame->height==2184&&frame->format==cpu->format);
            assert(frame->color_primaries==AVCOL_PRI_BT2020&&frame->color_trc==AVCOL_TRC_SMPTE2084);
            assert(left==cases[i].strip_left&&strip.width==cases[i].width&&strip.height==8);
            for(int row=0;row<4;row++)for(int y=0;y<2;y++)for(int x=0;x<strip.width;x++){
                int source_y=(av_clip(cases[i].rows[row],0,2183)&~1)+y;
                assert(luma_sample(&strip,x,row*2+y)==luma_sample(cpu,left+x,source_y));
            }
            uint32_t sequence=0;uint64_t ticks=0;
            assert(picture_timestamp(frame,&strip,left,&sequence,&ticks)==cases[i].valid);
            if(cases[i].valid)assert(sequence==words[0]&&ticks==UINT64_C(0x9abcdef012345678));
            unmap_barcode(decoded);av_frame_unref(frame);
        }
        video_cleanup();assert(!barcode_staging);
        av_frame_free(&frame);av_frame_free(&cpu);av_frame_free(&other);av_frame_free(&decoded);av_buffer_unref(&pool);
    }
    av_buffer_unref(&device);
    barcode_bottom=0;barcode_scale=1;barcode_left=0;barcode_bottom_margin=0;
    puts("D3D11 NV12/P010 barcode readback tests passed");
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
    test_audio();test_measurements();test_d3d11_readback();
    puts("Receiver picture and time-series tests passed");
    return 0;
}
