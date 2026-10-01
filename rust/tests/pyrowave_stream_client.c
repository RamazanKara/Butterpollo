// Independent Nonary Moonlight transport + vendor decode fixture. Not host code.
#include <Limelight.h>
#include <windows.h>
#include <vulkan/vulkan.h>
#include <pyrowave/pyrowave.h>
#include <opus/opus_multistream.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdatomic.h>
#include <stdarg.h>
#include <string.h>
static pyrowave_device device;
static pyrowave_decoder decoder;
static pyrowave_cpu_buffer output;
static unsigned char *pixels;
static OpusMSDecoder *opus;
static atomic_int frames, decoded, audio_packets, failures, ended, partial, hdr_frames;
static int requested, width, height;
static double latencies[100000], decode_times[100000], last_decoded_ms;
static unsigned measured;
static double clock_ms(void) { LARGE_INTEGER n,f;QueryPerformanceCounter(&n);QueryPerformanceFrequency(&f);return (double)n.QuadPart*1000/f.QuadPart; }
static uint32_t le(const unsigned char *p) { return (uint32_t)p[0]|((uint32_t)p[1]<<8)|((uint32_t)p[2]<<16)|((uint32_t)p[3]<<24); }
static int compare(const void *a,const void *b) { double x=*(const double*)a,y=*(const double*)b;return (x>y)-(x<y); }
static int video_setup(int format,int w,int h,int rate,void *context,int flags) {
    (void)context;(void)flags;
    printf("VIDEO format=%d %dx%d@%d\n",format,w,h,rate);
    if(format!=requested || w!=width || h!=height)return -1;
    if(pyrowave_create_device_by_compat(0,0,NULL,NULL,NULL,&device))return -1;
    int chroma=(format&VIDEO_FORMAT_MASK_YUV444)!=0;
    pyrowave_decoder_create_info info={.device=device,.width=w,.height=h,.chroma=chroma,.fragment_path=false};
    if(pyrowave_decoder_create(&info,&decoder))return -1;
    size_t y=(size_t)w*h,c=y/(chroma?1:4);
    pixels=malloc(y+2*c);if(!pixels)return -1;
    output=(pyrowave_cpu_buffer){.data={pixels,pixels+y,pixels+y+c},.row_stride_in_bytes={w,w/(chroma?1:2),w/(chroma?1:2)},.plane_size_in_bytes={y,c,c},.width=w,.height=h,.format=chroma?PYROWAVE_CPU_BUFFER_FORMAT_YUV444P:PYROWAVE_CPU_BUFFER_FORMAT_YUV420P};
    return 0;
}
static int video_frame(PDECODE_UNIT unit) {
    double started=clock_ms();
    if(unit->fullLength<8 || unit->fullLength>32*1024*1024) { atomic_fetch_add(&failures,1);return DR_OK; }
    unsigned char *data=malloc(unit->fullLength);if(!data) { atomic_fetch_add(&failures,1);return DR_OK; }
    size_t offset=0;int lost=0;
    for(PLENTRY entry=unit->bufferList;entry;entry=entry->next) {
        if(entry->length<=0 || offset+(size_t)entry->length>(size_t)unit->fullLength) { free(data);atomic_fetch_add(&failures,1);return DR_OK; }
        memcpy(data+offset,entry->data,entry->length);offset+=entry->length;
        lost+=entry->bufferType==BUFFER_TYPE_LOST;
    }
    if(offset!=(size_t)unit->fullLength || lost) { free(data);atomic_fetch_add(&partial,1);return DR_OK; }
    size_t length=offset;offset=0;
    while(offset<length) {
        if(length-offset<8)break;
        uint32_t header=le(data+offset);
        size_t size=header==UINT32_MAX?8+(size_t)le(data+offset+4)*4:(header&0x80000000)?8:((header>>16)&0xfff)*4;
        if(size<8 || size>length-offset)break;
        if(header!=UINT32_MAX && pyrowave_decoder_push_packet(decoder,data+offset,size))break;
        offset+=size;
    }
    if(offset!=length || !pyrowave_decoder_decode_is_ready(decoder,false) || pyrowave_decoder_decode_cpu_buffer_synchronous(decoder,&output)) {
        atomic_fetch_add(&failures,1);
    } else {
        int number=atomic_fetch_add(&decoded,1)+1;
        last_decoded_ms=clock_ms();
        if(unit->hdrActive)atomic_fetch_add(&hdr_frames,1);
        if(number<=3)printf("DECODED %dx%d frame=%d bytes=%d critical=%u hdr=%d\n",width,height,unit->frameNumber,unit->fullLength,unit->pyrowaveCriticalPackets,unit->hdrActive);
    }
    free(data);atomic_fetch_add(&frames,1);
    if(measured<100000) { latencies[measured]=unit->frameHostProcessingLatency/10.;decode_times[measured]=clock_ms()-started;measured++; }
    return DR_OK;
}
static int audio_init(int config,const POPUS_MULTISTREAM_CONFIGURATION layout,void *context,int flags) {
    (void)config;(void)context;(void)flags;int error;
    opus=opus_multistream_decoder_create(48000,layout->channelCount,layout->streams,layout->coupledStreams,layout->mapping,&error);return error;
}
static void audio_frame(char *data,int size) { float samples[5760*8];if(opus_multistream_decode_float(opus,(unsigned char*)data,size,samples,5760,0)>0)atomic_fetch_add(&audio_packets,1);else atomic_fetch_add(&failures,1); }
static void stage_start(int stage) { printf("STAGE %s\n",LiGetStageName(stage)); }
static void stage_failed(int stage,int error) { printf("FAILED %s error=%d\n",LiGetStageName(stage),error); }
static void terminated(int error) { printf("TERMINATED error=%d\n",error);atomic_store(&ended,1); }
static void log_message(const char *format,...) { va_list args;va_start(args,format);vprintf(format,args);va_end(args); }
int main(int argc,char **argv) {
    if(argc<3)return 2;
    setbuf(stdout,NULL);
    requested=VIDEO_FORMAT_PYROWAVE;
    if(!strcmp(argv[2],"pyrowave-444"))requested=VIDEO_FORMAT_PYROWAVE_444;
    else if(!strcmp(argv[2],"pyrowave-hdr"))requested=VIDEO_FORMAT_PYROWAVE_HDR10;
    else if(!strcmp(argv[2],"pyrowave-hdr-444"))requested=VIDEO_FORMAT_PYROWAVE_HDR10_444;
    else if(strcmp(argv[2],"pyrowave"))return 2;
    width=argc>3?atoi(argv[3]):640;height=argc>4?atoi(argv[4]):480;
    int fps=argc>5?atoi(argv[5]):60,duration=argc>6?atoi(argv[6]):10,bitrate=argc>7?atoi(argv[7]):200000;
    if(width<2 || width>4096 || height<2 || height>4096 || fps<1 || fps>240 || duration<1 || duration>300 || bitrate<100 || bitrate>2000000)return 2;
    SERVER_INFORMATION server;LiInitializeServerInformation(&server);
    server.address="127.0.0.1";server.serverInfoAppVersion="7.1.431.-1";server.serverInfoGfeVersion="3.23.0.74";server.rtspSessionUrl=argv[1];server.serverCodecModeSupport=SCM_MASK_PYROWAVE;
    STREAM_CONFIGURATION config;LiInitializeStreamConfiguration(&config);
    config.width=width;config.height=height;config.fps=fps;config.bitrate=bitrate;config.packetSize=1392;config.streamingRemotely=STREAM_CFG_LOCAL;
    config.audioConfiguration=AUDIO_CONFIGURATION_STEREO;config.supportedVideoFormats=requested;config.encryptionFlags=ENCFLG_ALL;
    for(int i=0;i<16;i++)config.remoteInputAesKey[i]=(char)i;
    config.remoteInputAesIv[3]=123;
    CONNECTION_LISTENER_CALLBACKS listener;LiInitializeConnectionCallbacks(&listener);listener.stageStarting=stage_start;listener.stageFailed=stage_failed;listener.connectionTerminated=terminated;listener.logMessage=log_message;
    DECODER_RENDERER_CALLBACKS video;LiInitializeVideoCallbacks(&video);video.setup=video_setup;video.submitDecodeUnit=video_frame;
    AUDIO_RENDERER_CALLBACKS audio;LiInitializeAudioCallbacks(&audio);audio.init=audio_init;audio.decodeAndPlaySample=audio_frame;audio.capabilities=CAPABILITY_DIRECT_SUBMIT;
    int result=LiStartConnection(&server,&config,&listener,&video,&audio,NULL,0,NULL,0);
    if(result || strcmp(LiGetHostPyroWaveBitstreamId(),PYROWAVE_BITSTREAM_ID)) { printf("CONNECT FAILED result=%d bitstream=%s\n",result,LiGetHostPyroWaveBitstreamId());LiStopConnection();return 1; }
    double started=clock_ms();for(int i=0;i<duration*10 && !atomic_load(&ended);i++)Sleep(100);
    double finished=clock_ms(),seconds=(finished-started)/1000.;int premature=atomic_load(&ended);LiStopConnection();
    printf("RESULT frames=%d decoded_frames=%d partial_frames=%d hdr_frames=%d audio_packets=%d failures=%d\n",atomic_load(&frames),atomic_load(&decoded),atomic_load(&partial),atomic_load(&hdr_frames),atomic_load(&audio_packets),atomic_load(&failures));
    if(measured) {
        double host_sum=0,decode_sum=0;for(unsigned i=0;i<measured;i++) { host_sum+=latencies[i];decode_sum+=decode_times[i]; }
        qsort(latencies,measured,sizeof(double),compare);
        printf("PERFORMANCE seconds=%.3f received_fps=%.2f decoded_fps=%.2f host_mean_ms=%.3f host_p95_ms=%.3f decoder_mean_ms=%.3f\n",seconds,atomic_load(&frames)/seconds,atomic_load(&decoded)/seconds,host_sum/measured,latencies[(measured-1)*95/100],decode_sum/measured);
    }
    if(decoder)pyrowave_decoder_destroy(decoder);
    if(device)pyrowave_device_destroy(device);
    if(opus)opus_multistream_decoder_destroy(opus);
    free(pixels);
    int hdr_ok=(requested&VIDEO_FORMAT_MASK_10BIT)?atomic_load(&hdr_frames)>=30:atomic_load(&hdr_frames)==0;
    return !premature && hdr_ok && atomic_load(&decoded)>=fps*duration/2 && finished-last_decoded_ms<1000 && atomic_load(&audio_packets)>0 && atomic_load(&failures)==0?0:1;
}
