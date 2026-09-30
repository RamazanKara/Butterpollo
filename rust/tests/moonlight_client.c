/* Interoperability probe: Moonlight's independent C client validates Rust's wire data.
 * This is a test executable, never linked into the Rust host. */
#include <Limelight.h>
#include <windows.h>
#include <stdio.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <string.h>
#include <libavcodec/avcodec.h>
#include <opus/opus_multistream.h>
static atomic_int frames, decoded_frames, audio_packets, ended, failures;
static AVCodecContext *decoder;
static OpusMSDecoder *opus_decoder;
static int audio_channels;
static int video_setup(int format,int width,int height,int rate,void*context,int flags){
    printf("VIDEO format=%d %dx%d@%d\n",format,width,height,rate);
    enum AVCodecID id=(format&VIDEO_FORMAT_MASK_H264)?AV_CODEC_ID_H264:(format&VIDEO_FORMAT_MASK_H265)?AV_CODEC_ID_HEVC:AV_CODEC_ID_AV1;
    const AVCodec *codec=avcodec_find_decoder(id);if(!codec){fprintf(stderr,"Independent decoder unavailable for codec %d\n",id);return -1;}
    decoder=avcodec_alloc_context3(codec);if(!decoder)return -1;decoder->thread_count=1;return avcodec_open2(decoder,codec,NULL);
}
static int video_frame(PDECODE_UNIT unit){
    AVPacket *packet=av_packet_alloc();AVFrame *frame=av_frame_alloc();av_new_packet(packet,unit->fullLength);
    int offset=0;for(PLENTRY entry=unit->bufferList;entry;entry=entry->next){memcpy(packet->data+offset,entry->data,entry->length);offset+=entry->length;}
    if(avcodec_send_packet(decoder,packet)<0)atomic_fetch_add(&failures,1);
    while(avcodec_receive_frame(decoder,frame)==0){
        atomic_fetch_add(&decoded_frames,1);if(atomic_load(&decoded_frames)==1)printf("DECODED %dx%d pixel_format=%d primaries=%d transfer=%d\n",frame->width,frame->height,frame->format,frame->color_primaries,frame->color_trc);
        av_frame_unref(frame);
    }
    av_packet_free(&packet);av_frame_free(&frame);
    atomic_fetch_add(&frames,1);if(atomic_load(&frames)<4)printf("FRAME %d bytes=%d type=%d\n",unit->frameNumber,unit->fullLength,unit->frameType);return DR_OK;
}
static int audio_init(int config,const POPUS_MULTISTREAM_CONFIGURATION opus,void*context,int flags){
    printf("AUDIO channels=%d samples=%d\n",opus->channelCount,opus->samplesPerFrame);
    int error;audio_channels=opus->channelCount;opus_decoder=opus_multistream_decoder_create(48000,audio_channels,opus->streams,opus->coupledStreams,opus->mapping,&error);return error;
}
static void audio_frame(char*data,int size){
    float samples[5760*8];if(opus_multistream_decode_float(opus_decoder,(unsigned char*)data,size,samples,5760,0)>0)atomic_fetch_add(&audio_packets,1);else atomic_fetch_add(&failures,1);
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
    for(int i=0;i<16;i++)config.remoteInputAesKey[i]=(char)i;config.remoteInputAesIv[3]=123;
    CONNECTION_LISTENER_CALLBACKS listener;LiInitializeConnectionCallbacks(&listener);listener.stageStarting=stage_start;listener.stageFailed=stage_failed;listener.connectionTerminated=terminated;listener.logMessage=log_message;
    DECODER_RENDERER_CALLBACKS video;LiInitializeVideoCallbacks(&video);video.setup=video_setup;video.submitDecodeUnit=video_frame;video.capabilities=CAPABILITY_DIRECT_SUBMIT;
    AUDIO_RENDERER_CALLBACKS audio;LiInitializeAudioCallbacks(&audio);audio.init=audio_init;audio.decodeAndPlaySample=audio_frame;audio.capabilities=CAPABILITY_DIRECT_SUBMIT;
    int result=LiStartConnection(&server,&config,&listener,&video,&audio,NULL,0,NULL,0);
    if(result){printf("CONNECT FAILED %d\n",result);return 1;}
    for(int i=0;i<100&&!atomic_load(&ended)&&atomic_load(&frames)<30;i++)Sleep(100);
    LiStopConnection();printf("RESULT frames=%d decoded_frames=%d audio_packets=%d failures=%d\n",atomic_load(&frames),atomic_load(&decoded_frames),atomic_load(&audio_packets),atomic_load(&failures));
    avcodec_free_context(&decoder);if(opus_decoder)opus_multistream_decoder_destroy(opus_decoder);
    return atomic_load(&decoded_frames)>=30&&atomic_load(&audio_packets)>0&&atomic_load(&failures)==0?0:1;
}
