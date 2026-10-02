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
static unsigned measured_frames;
static double clock_ms(void){LARGE_INTEGER n,f;QueryPerformanceCounter(&n);QueryPerformanceFrequency(&f);return (double)n.QuadPart*1000.0/(double)f.QuadPart;}
static int compare_double(const void*a,const void*b){double x=*(const double*)a,y=*(const double*)b;return(x>y)-(x<y);}
static int video_setup(int format,int width,int height,int rate,void*context,int flags){
    printf("VIDEO format=%d %dx%d@%d\n",format,width,height,rate);
    if(format != requested_format){fprintf(stderr,"Codec fallback: requested=%d negotiated=%d\n",requested_format,format);return -1;}
    enum AVCodecID id=(format&VIDEO_FORMAT_MASK_H264)?AV_CODEC_ID_H264:(format&VIDEO_FORMAT_MASK_H265)?AV_CODEC_ID_HEVC:AV_CODEC_ID_AV1;
    const AVCodec *codec=avcodec_find_decoder(id);if(!codec){fprintf(stderr,"Independent decoder unavailable for codec %d\n",id);return -1;}
    decoder=avcodec_alloc_context3(codec);if(!decoder)return -1;decoder->thread_count=decoder_threads;return avcodec_open2(decoder,codec,NULL);
}
static int video_frame(PDECODE_UNIT unit){
    double decode_started=clock_ms();
    AVPacket *packet=av_packet_alloc();AVFrame *frame=av_frame_alloc();av_new_packet(packet,unit->fullLength);
    int offset=0;for(PLENTRY entry=unit->bufferList;entry;entry=entry->next){memcpy(packet->data+offset,entry->data,entry->length);offset+=entry->length;}
    if(avcodec_send_packet(decoder,packet)<0)atomic_fetch_add(&failures,1);
    int received;
    while((received=avcodec_receive_frame(decoder,frame))==0){
        if(frame->width!=requested_width||frame->height!=requested_height)atomic_fetch_add(&failures,1);
        if(requested_hdr){const AVPixFmtDescriptor *desc=av_pix_fmt_desc_get(frame->format);if(!desc||desc->comp[0].depth<10||frame->color_primaries!=AVCOL_PRI_BT2020||frame->color_trc!=AVCOL_TRC_SMPTE2084)atomic_fetch_add(&failures,1);}
        atomic_fetch_add(&decoded_frames,1);if(atomic_load(&decoded_frames)==1)printf("DECODED %dx%d pixel_format=%d primaries=%d transfer=%d\n",frame->width,frame->height,frame->format,frame->color_primaries,frame->color_trc);
        av_frame_unref(frame);
    }
    if(received!=AVERROR(EAGAIN)&&received!=AVERROR_EOF)atomic_fetch_add(&failures,1);
    av_packet_free(&packet);av_frame_free(&frame);
    if(measured_frames<100000){host_latency[measured_frames]=unit->frameHostProcessingLatency/10.0;decode_time_ms[measured_frames]=clock_ms()-decode_started;measured_frames++;}
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
    if(argc>3)config.width=atoi(argv[3]);if(argc>4)config.height=atoi(argv[4]);if(argc>5)config.fps=atoi(argv[5]);if(argc>7)config.bitrate=atoi(argv[7]);
    if(argc>8)decoder_threads=atoi(argv[8]);requested_width=config.width;requested_height=config.height;
    if(config.width<2||config.width>8192||config.height<2||config.height>8192||config.fps<1||config.fps>240||duration<0||duration>300||decoder_threads<1||decoder_threads>16)return 2;
    printf("DECODER threads=%d\n",decoder_threads);
    for(int i=0;i<16;i++)config.remoteInputAesKey[i]=(char)i;config.remoteInputAesIv[3]=123;
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
    printf("AUDIO_SIGNAL samples=%llu peak=%.6f rms=%.6f\n",audio_samples,audio_peak,audio_samples?sqrt(audio_energy/audio_samples):0.0);
    if(duration&&measured_frames){
        double host_sum=0,decode_sum=0;for(unsigned i=0;i<measured_frames;i++){host_sum+=host_latency[i];decode_sum+=decode_time_ms[i];}
        qsort(host_latency,measured_frames,sizeof(double),compare_double);
        printf("PERFORMANCE seconds=%.3f received_fps=%.2f decoded_fps=%.2f host_mean_ms=%.3f host_p50_ms=%.3f host_p95_ms=%.3f decoder_mean_ms=%.3f\n",seconds,atomic_load(&frames)/seconds,atomic_load(&decoded_frames)/seconds,host_sum/measured_frames,host_latency[(measured_frames-1)/2],host_latency[(measured_frames-1)*95/100],decode_sum/measured_frames);
    }
    avcodec_free_context(&decoder);if(opus_decoder)opus_multistream_decoder_destroy(opus_decoder);
    return !premature&&atomic_load(&decoded_frames)>=30&&atomic_load(&audio_packets)>0&&atomic_load(&failures)==0&&(!getenv("BUTTERPOLLO_TEST_AUDIO_TONE")||audio_peak>0.01)?0:1;
}
