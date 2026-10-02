/* Interoperability probe: Moonlight's independent C client validates Rust's wire data.
 * This is a test executable, never linked into the Rust host. */
#include <Limelight.h>
#include <windows.h>
#include <stdio.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <string.h>
#include <stdlib.h>
#include <math.h>
#include <libavcodec/avcodec.h>
#include <libavutil/pixdesc.h>
#include <opus/opus_multistream.h>
static atomic_int frames, decoded_frames, audio_packets, ended, failures;
static AVCodecContext *decoder;
static OpusMSDecoder *opus_decoder;
static int audio_channels;
static int requested_format;
static int requested_hdr;
static int decoder_threads=1;
static int requested_width,requested_height;
static double audio_energy,audio_peak;
static unsigned long long audio_samples;
static double host_latency[100000], decode_time_ms[100000];
static double arrivals[100000], picture_age[100000];
static double assembly_times[100000];
static uint32_t picture_frames[100000];
static unsigned measured_frames;
static FILE *timing_csv;
static double warmup_seconds=2.0;
static int barcode_bottom;
/* The host may scale the source; the barcode is drawn at source pixels. */
static double barcode_scale=1.0;
static double clock_ms(void){LARGE_INTEGER n,f;QueryPerformanceCounter(&n);QueryPerformanceFrequency(&f);return (double)n.QuadPart*1000.0/(double)f.QuadPart;}
static int compare_double(const void*a,const void*b){double x=*(const double*)a,y=*(const double*)b;return(x>y)-(x<y);}
static unsigned luma_sample(const AVFrame *frame,int x,int y){
    const AVPixFmtDescriptor *desc=av_pix_fmt_desc_get(frame->format);
    if(!desc||desc->comp[0].plane!=0||x>=frame->width||y>=frame->height)return 0;
    const AVComponentDescriptor *component=&desc->comp[0];
    const unsigned char *pixel=frame->data[0]+y*frame->linesize[0]+x*component->step+component->offset;
    unsigned value=pixel[0];if(component->depth>8)value|=(unsigned)pixel[1]<<8;
    return(value>>component->shift)&((1u<<component->depth)-1u);
}
static int picture_timestamp(const AVFrame *frame,uint32_t *sequence,uint64_t *ticks){
    const double s=barcode_scale;
    if(frame->width<640*s||frame->height<128*s)return 0;
    uint32_t words[4]={0};
    for(int row=0;row<4;row++){
        int y=barcode_bottom?frame->height-(int)((128-20-row*24)*s):(int)((20+row*24)*s);
        unsigned black=luma_sample(frame,(int)(12*s),y),white=luma_sample(frame,(int)(36*s),y);
        if(white<=black+32)return 0;
        unsigned threshold=(black+white)/2;
        for(int bit=0;bit<32;bit++)if(luma_sample(frame,(int)((72+bit*16)*s),y)>threshold)words[row]|=1u<<bit;
    }
    if(words[3]!=0xB17E2212||words[0]==0)return 0;
    *sequence=words[0];*ticks=((uint64_t)words[2]<<32)|words[1];return 1;
}
static void distribution(const char *name,double *values,unsigned count){
    if(!count)return;
    double sum=0;for(unsigned i=0;i<count;i++)sum+=values[i];
    qsort(values,count,sizeof(double),compare_double);
    printf("%s samples=%u mean_ms=%.3f p50_ms=%.3f p95_ms=%.3f p99_ms=%.3f max_ms=%.3f\n",name,count,sum/count,values[(count-1)/2],values[(count-1)*95/100],values[(count-1)*99/100],values[count-1]);
}
static int video_setup(int format,int width,int height,int rate,void*context,int flags){
    printf("VIDEO format=%d %dx%d@%d\n",format,width,height,rate);
    if(format != requested_format){fprintf(stderr,"Codec fallback: requested=%d negotiated=%d\n",requested_format,format);return -1;}
    enum AVCodecID id=(format&VIDEO_FORMAT_MASK_H264)?AV_CODEC_ID_H264:(format&VIDEO_FORMAT_MASK_H265)?AV_CODEC_ID_HEVC:AV_CODEC_ID_AV1;
    const AVCodec *codec=avcodec_find_decoder(id);if(!codec){fprintf(stderr,"Independent decoder unavailable for codec %d\n",id);return -1;}
    decoder=avcodec_alloc_context3(codec);if(!decoder)return -1;
    decoder->thread_count=decoder_threads;decoder->thread_type=FF_THREAD_SLICE;decoder->flags|=AV_CODEC_FLAG_LOW_DELAY;
    AVDictionary *options=NULL;
    // FFmpeg's default frame threading buffers several whole pictures. Keep
    // this independent streaming decoder parallel within a picture instead.
    if(strcmp(codec->name,"libdav1d")==0)av_dict_set(&options,"max_frame_delay","1",0);
    int result=avcodec_open2(decoder,codec,&options);av_dict_free(&options);
    printf("DECODER codec=%s active_thread_type=%d delay=%d\n",codec->name,decoder->active_thread_type,decoder->delay);
    return result;
}
static int video_frame(PDECODE_UNIT unit){
    double decode_started=clock_ms();
    uint32_t picture_sequence=0;uint64_t picture_ticks=0;double age_ms=-1;
    AVPacket *packet=av_packet_alloc();AVFrame *frame=av_frame_alloc();av_new_packet(packet,unit->fullLength);
    int offset=0;for(PLENTRY entry=unit->bufferList;entry;entry=entry->next){memcpy(packet->data+offset,entry->data,entry->length);offset+=entry->length;}
    // Optional first access-unit dump runs before the steady measurement window.
    // It allows an independent bitstream parser to diagnose driver geometry.
    const char *dump=getenv("BUTTERPOLLO_TEST_FIRST_FRAME");
    if(dump&&atomic_load(&frames)==0){FILE *file=fopen(dump,"wb");if(!file||fwrite(packet->data,1,packet->size,file)!=(size_t)packet->size)atomic_fetch_add(&failures,1);if(file)fclose(file);}
    if(avcodec_send_packet(decoder,packet)<0)atomic_fetch_add(&failures,1);
    int received;
    while((received=avcodec_receive_frame(decoder,frame))==0){
        if(frame->width!=requested_width||frame->height!=requested_height)atomic_fetch_add(&failures,1);
        if(requested_hdr){const AVPixFmtDescriptor *desc=av_pix_fmt_desc_get(frame->format);if(!desc||desc->comp[0].depth<10||frame->color_primaries!=AVCOL_PRI_BT2020||frame->color_trc!=AVCOL_TRC_SMPTE2084)atomic_fetch_add(&failures,1);}
        atomic_fetch_add(&decoded_frames,1);if(atomic_load(&decoded_frames)==1)printf("DECODED %dx%d pixel_format=%d primaries=%d transfer=%d\n",frame->width,frame->height,frame->format,frame->color_primaries,frame->color_trc);
        if(picture_timestamp(frame,&picture_sequence,&picture_ticks)){
            LARGE_INTEGER frequency;QueryPerformanceFrequency(&frequency);
            age_ms=clock_ms()-(double)picture_ticks*1000.0/(double)frequency.QuadPart;
            // A signature alone must not turn unrelated desktop content into a
            // latency sample. Both processes use the same Windows QPC clock.
            if(age_ms<0||age_ms>3000){picture_sequence=0;age_ms=-1;}
        }
        av_frame_unref(frame);
    }
    if(received!=AVERROR(EAGAIN)&&received!=AVERROR_EOF)atomic_fetch_add(&failures,1);
    av_packet_free(&packet);av_frame_free(&frame);
    double decode_ms=clock_ms()-decode_started;
    if(measured_frames<100000){
        host_latency[measured_frames]=unit->frameHostProcessingLatency/10.0;decode_time_ms[measured_frames]=decode_ms;
        arrivals[measured_frames]=decode_started;assembly_times[measured_frames]=unit->enqueueTimeUs/1000.0;picture_age[measured_frames]=age_ms;picture_frames[measured_frames]=picture_sequence;measured_frames++;
    }
    if(timing_csv)fprintf(timing_csv,"%d,%.6f,%.3f,%.6f,%u,%llu,%.6f,%llu,%llu,%llu\n",unit->frameNumber,decode_started,unit->frameHostProcessingLatency/10.0,decode_ms,picture_sequence,(unsigned long long)picture_ticks,age_ms,(unsigned long long)unit->receiveTimeUs,(unsigned long long)unit->enqueueTimeUs,(unsigned long long)unit->presentationTimeUs);
    atomic_fetch_add(&frames,1);if(atomic_load(&frames)<4)printf("FRAME %d bytes=%d type=%d\n",unit->frameNumber,unit->fullLength,unit->frameType);return DR_OK;
}
static int audio_init(int config,const POPUS_MULTISTREAM_CONFIGURATION opus,void*context,int flags){
    printf("AUDIO channels=%d samples=%d\n",opus->channelCount,opus->samplesPerFrame);
    int error;audio_channels=opus->channelCount;opus_decoder=opus_multistream_decoder_create(48000,audio_channels,opus->streams,opus->coupledStreams,opus->mapping,&error);return error;
}
static void audio_frame(char*data,int size){
    float samples[5760*8];int count=opus_multistream_decode_float(opus_decoder,(unsigned char*)data,size,samples,5760,0);
    if(count>0){
        atomic_fetch_add(&audio_packets,1);
        for(int i=0;i<count*audio_channels;i++){double v=samples[i];audio_energy+=v*v;if(fabs(v)>audio_peak)audio_peak=fabs(v);audio_samples++;}
    }else atomic_fetch_add(&failures,1);
}
static void stage_start(int stage){printf("STAGE %s\n",LiGetStageName(stage));}
static void stage_failed(int stage,int error){printf("FAILED %s error=%d\n",LiGetStageName(stage),error);}
static void terminated(int error){printf("TERMINATED error=%d\n",error);atomic_store(&ended,1);}
static void log_message(const char*fmt,...){va_list args;va_start(args,fmt);vprintf(fmt,args);va_end(args);}
int main(int argc,char**argv){
    if(argc<2){fprintf(stderr,"session URL required\n");return 2;}
    setbuf(stdout,NULL);
    SERVER_INFORMATION server;LiInitializeServerInformation(&server);
    server.address="127.0.0.1";server.serverInfoAppVersion="7.1.431.-1";server.serverInfoGfeVersion="3.23.0.74";server.rtspSessionUrl=argv[1];server.serverCodecModeSupport=0x30301;
    STREAM_CONFIGURATION config;LiInitializeStreamConfiguration(&config);config.width=640;config.height=480;config.fps=30;config.bitrate=2000;config.packetSize=1024;config.streamingRemotely=STREAM_CFG_LOCAL;config.audioConfiguration=AUDIO_CONFIGURATION_STEREO;config.supportedVideoFormats=VIDEO_FORMAT_H264;config.encryptionFlags=ENCFLG_ALL;
    if(argc>2){if(strcmp(argv[2],"hevc")==0)config.supportedVideoFormats=VIDEO_FORMAT_H265;else if(strcmp(argv[2],"hevc-hdr")==0)config.supportedVideoFormats=VIDEO_FORMAT_H265_MAIN10;else if(strcmp(argv[2],"av1")==0)config.supportedVideoFormats=VIDEO_FORMAT_AV1_MAIN8;else if(strcmp(argv[2],"av1-hdr")==0)config.supportedVideoFormats=VIDEO_FORMAT_AV1_MAIN10;}
    requested_format=config.supportedVideoFormats;requested_hdr=(requested_format&(VIDEO_FORMAT_H265_MAIN10|VIDEO_FORMAT_AV1_MAIN10))!=0;
    int duration=argc>6?atoi(argv[6]):0;
    if(getenv("BUTTERPOLLO_TEST_WARMUP_SECONDS"))warmup_seconds=atof(getenv("BUTTERPOLLO_TEST_WARMUP_SECONDS"));
    barcode_bottom=getenv("BUTTERPOLLO_TEST_BARCODE_BOTTOM")&&strcmp(getenv("BUTTERPOLLO_TEST_BARCODE_BOTTOM"),"1")==0;
    if(getenv("BUTTERPOLLO_TEST_BARCODE_SCALE"))barcode_scale=atof(getenv("BUTTERPOLLO_TEST_BARCODE_SCALE"));
    if(!(barcode_scale>0.0&&barcode_scale<=1.0))barcode_scale=1.0;
    if(warmup_seconds<0||warmup_seconds>60)return 2;
    if(getenv("BUTTERPOLLO_TEST_TIMING_CSV")){
        timing_csv=fopen(getenv("BUTTERPOLLO_TEST_TIMING_CSV"),"w");if(!timing_csv){perror("timing CSV");return 2;}
        fprintf(timing_csv,"wire_frame,arrival_ms,host_ms,decode_ms,render_frame,render_qpc,picture_age_ms,first_packet_us,assembled_us,presentation_us\n");
    }
    if(argc>3)config.width=atoi(argv[3]);if(argc>4)config.height=atoi(argv[4]);if(argc>5)config.fps=atoi(argv[5]);if(argc>7)config.bitrate=atoi(argv[7]);
    if(argc>8)decoder_threads=atoi(argv[8]);requested_width=config.width;requested_height=config.height;
    if(config.width<2||config.width>8192||config.height<2||config.height>8192||config.fps<1||config.fps>240||duration<0||duration>300||decoder_threads<1||decoder_threads>16)return 2;
    printf("DECODER threads=%d\n",decoder_threads);
    for(int i=0;i<16;i++)config.remoteInputAesKey[i]=(char)i;config.remoteInputAesIv[3]=123;
    if(getenv("BUTTERPOLLO_TEST_SIGNED_KEY_ID")&&strcmp(getenv("BUTTERPOLLO_TEST_SIGNED_KEY_ID"),"1")==0)config.remoteInputAesIv[0]=(char)0x80;
    CONNECTION_LISTENER_CALLBACKS listener;LiInitializeConnectionCallbacks(&listener);listener.stageStarting=stage_start;listener.stageFailed=stage_failed;listener.connectionTerminated=terminated;listener.logMessage=log_message;
    DECODER_RENDERER_CALLBACKS video;LiInitializeVideoCallbacks(&video);video.setup=video_setup;video.submitDecodeUnit=video_frame;video.capabilities=CAPABILITY_DIRECT_SUBMIT;
    AUDIO_RENDERER_CALLBACKS audio;LiInitializeAudioCallbacks(&audio);audio.init=audio_init;audio.decodeAndPlaySample=audio_frame;audio.capabilities=CAPABILITY_DIRECT_SUBMIT;
    int result=LiStartConnection(&server,&config,&listener,&video,&audio,NULL,0,NULL,0);
    if(result){printf("CONNECT FAILED %d\n",result);return 1;}
    double started=clock_ms();
    for(int i=0;i<(duration?duration*10:100)&&!atomic_load(&ended)&&(duration||atomic_load(&frames)<30);i++)Sleep(100);
    double seconds=(clock_ms()-started)/1000.0;
    int premature=atomic_load(&ended)||(duration&&seconds<duration*0.98);
    LiStopConnection();printf("RESULT frames=%d decoded_frames=%d audio_packets=%d failures=%d\n",atomic_load(&frames),atomic_load(&decoded_frames),atomic_load(&audio_packets),atomic_load(&failures));
    if(timing_csv)fclose(timing_csv);
    printf("AUDIO_SIGNAL samples=%llu peak=%.6f rms=%.6f\n",audio_samples,audio_peak,audio_samples?sqrt(audio_energy/audio_samples):0.0);
    int motion_valid=!getenv("BUTTERPOLLO_TEST_REQUIRE_MOTION");
    if(duration&&measured_frames){
        double host_sum=0,decode_sum=0;for(unsigned i=0;i<measured_frames;i++){host_sum+=host_latency[i];decode_sum+=decode_time_ms[i];}
        static double steady_host[100000],intervals[100000],ages[100000];
        unsigned steady_count=0,interval_count=0,visual_count=0,repeats=0,skips=0,unique=0,late=0;
        uint32_t last_picture=0;double first_steady=0,last_steady=0;
        for(unsigned i=0;i<measured_frames;i++)if(arrivals[i]>=arrivals[0]+warmup_seconds*1000){
            steady_host[steady_count++]=host_latency[i];
            if(!first_steady)first_steady=arrivals[i];last_steady=arrivals[i];
            if(i){double interval=assembly_times[i]-assembly_times[i-1];intervals[interval_count++]=interval;if(interval>1500.0/config.fps)late++;}
            if(picture_frames[i]){
                ages[visual_count++]=picture_age[i];
                if(picture_frames[i]==last_picture)repeats++;
                else{unique++;if(last_picture&&picture_frames[i]>last_picture+1)skips+=picture_frames[i]-last_picture-1;}
                last_picture=picture_frames[i];
            }
        }
        double steady_seconds=(last_steady-first_steady)/1000;
        printf("STEADY warmup_seconds=%.3f seconds=%.3f frames=%u fps=%.3f intervals_over_1_5_period=%u\n",warmup_seconds,steady_seconds,steady_count,steady_seconds>0?(steady_count-1)/steady_seconds:0,late);
        distribution("STEADY_HOST",steady_host,steady_count);distribution("ARRIVAL_INTERVAL",intervals,interval_count);
        if(visual_count){
            printf("VISUAL frames=%u unique=%u repeats=%u skipped_render_frames=%u unique_fps=%.3f coverage=%.6f\n",visual_count,unique,repeats,skips,steady_seconds>0?(unique?unique-1:0)/steady_seconds:0,steady_count?(double)visual_count/steady_count:0);
            distribution("PICTURE_AGE",ages,visual_count);
            if(steady_count&&visual_count>=steady_count*0.95&&unique>1)motion_valid=1;
        }
        qsort(host_latency,measured_frames,sizeof(double),compare_double);
        printf("PERFORMANCE seconds=%.3f received_fps=%.2f decoded_fps=%.2f host_mean_ms=%.3f host_p50_ms=%.3f host_p95_ms=%.3f decoder_mean_ms=%.3f\n",seconds,atomic_load(&frames)/seconds,atomic_load(&decoded_frames)/seconds,host_sum/measured_frames,host_latency[(measured_frames-1)/2],host_latency[(measured_frames-1)*95/100],decode_sum/measured_frames);
    }
    if(!motion_valid)fprintf(stderr,"Motion measurement failed: missing timestamps or static test content\n");
    avcodec_free_context(&decoder);if(opus_decoder)opus_multistream_decoder_destroy(opus_decoder);
    return !premature&&motion_valid&&atomic_load(&decoded_frames)>=30&&atomic_load(&audio_packets)>0&&atomic_load(&failures)==0&&(!getenv("BUTTERPOLLO_TEST_AUDIO_TONE")||audio_peak>0.01)?0:1;
}
