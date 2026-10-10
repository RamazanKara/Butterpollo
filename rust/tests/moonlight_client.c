/* Interoperability probe: Moonlight's independent C client validates Rust's wire data.
 * This is a test executable, never linked into the Rust host. */
#include <Limelight.h>
#ifdef _WIN32
#define COBJMACROS
#include <windows.h>
#include <libavutil/hwcontext_d3d11va.h>
#else
#include <time.h>
#endif
#include <errno.h>
#include <stdio.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <string.h>
#include <stdlib.h>
#include <math.h>
#include <libavcodec/avcodec.h>
#include <libavutil/pixdesc.h>
#include <libavutil/hwcontext.h>
#include <opus/opus_multistream.h>
#ifdef BUTTERPOLLO_PYROWAVE
#include <vulkan/vulkan.h>
#include <pyrowave/pyrowave.h>
static pyrowave_device pyro_device;
static pyrowave_decoder pyro_decoder;
static unsigned record_frames, partial_frames, hdr_frames;
/* The pinned transport owns these negotiated flags; requesting ENCFLG_ALL alone
 * would still permit a host to silently omit video encryption. */
extern uint32_t EncryptionFeaturesEnabled;
#endif
static atomic_int frames, decoded_frames, audio_packets, ended, failures, detailed_frames;
static AVCodecContext *decoder;
static enum AVPixelFormat hardware_format=AV_PIX_FMT_NONE;
static OpusMSDecoder *opus_decoder;
static int audio_channels;
static int requested_format;
static int requested_hdr;
/* Unset logs only; auto follows the existing hevc-hdr/av1-hdr CLI selection. */
static int expected_hdr_control=-1;
static atomic_int hdr_notifications,hdr_enabled_notifications,hdr_disabled_notifications;
static atomic_int hdr_control_mismatches,hdr_invalid_metadata;
static int decoder_threads=1;
static int requested_width,requested_height;
static double audio_energy,audio_peak;
static double audio_tone_min_energy=INFINITY,audio_tone_max_energy;
static unsigned audio_tone_blocks;
static unsigned long long audio_samples;
#define MAX_MEASUREMENTS (3600 * 240)
static double host_latency[MAX_MEASUREMENTS], decode_time_ms[MAX_MEASUREMENTS];
static double arrivals[MAX_MEASUREMENTS], picture_age[MAX_MEASUREMENTS];
static double assembly_times[MAX_MEASUREMENTS];
static uint32_t picture_frames[MAX_MEASUREMENTS];
static unsigned measured_frames;
static FILE *timing_csv;
static FILE *audio_csv;
#define MAX_IDR_PROBES 2000
#define IDR_PROBE_INTERVAL_US UINT64_C(1500000)
static int idr_probe_count,idr_probe_sent;
static uint64_t idr_probe_interval_us=IDR_PROBE_INTERVAL_US;
static atomic_int idr_probe_pending;
static struct {uint64_t request_us,arrival_us,decoded_us;int wire_frame;} idr_probes[MAX_IDR_PROBES];
static FILE *idr_csv;
static double warmup_seconds=2.0;
static int barcode_bottom;
/* The host may scale the source; the barcode is drawn at source pixels. */
static double barcode_scale=1.0;
static double barcode_left,barcode_bottom_margin;
#ifdef _WIN32
static ID3D11Texture2D *barcode_staging;
static double clock_ms(void){LARGE_INTEGER n,f;QueryPerformanceCounter(&n);QueryPerformanceFrequency(&f);return (double)n.QuadPart*1000.0/(double)f.QuadPart;}
static void wait_ms(unsigned milliseconds){Sleep(milliseconds);}
#else
static double clock_ms(void){struct timespec n;clock_gettime(CLOCK_MONOTONIC,&n);return (double)n.tv_sec*1000.0+n.tv_nsec/1000000.0;}
static void wait_ms(unsigned milliseconds){struct timespec n={milliseconds/1000,(milliseconds%1000)*1000000L};while(nanosleep(&n,&n)<0&&errno==EINTR){}}
#endif
static int compare_double(const void*a,const void*b){double x=*(const double*)a,y=*(const double*)b;return(x>y)-(x<y);}
static int parse_idr_probe(const char *text){
    if(!text)return 0;
    char *end;errno=0;long count=strtol(text,&end,10);
    if(errno||text[0]<'0'||text[0]>'9'||*end||count<1||count>MAX_IDR_PROBES)return -1;
    return (int)count;
}
/* Milliseconds between requests, 100 to 10000; a lossy link asks several times a second. */
static long parse_idr_probe_interval_ms(const char *text){
    if(!text)return (long)(IDR_PROBE_INTERVAL_US/1000);
    char *end;errno=0;long ms=strtol(text,&end,10);
    if(errno||text[0]<'0'||text[0]>'9'||*end||ms<100||ms>10000)return -1;
    return ms;
}
static void poll_idr_probe(uint64_t now_us,uint64_t started_us){
    if(idr_probe_sent>=idr_probe_count||atomic_load(&idr_probe_pending)||!atomic_load(&decoded_frames))return;
    if(now_us-started_us<warmup_seconds*1000000.0)return;
    if(idr_probe_sent&&now_us-idr_probes[idr_probe_sent-1].request_us<idr_probe_interval_us)return;
    idr_probes[idr_probe_sent].request_us=now_us;
    // Publishing the slot before the API call also covers an immediate response.
    atomic_store(&idr_probe_pending,++idr_probe_sent);
    LiRequestIdrFrame();
}
static void idr_probe_arrived(PDECODE_UNIT unit){
    int pending=atomic_load(&idr_probe_pending);
    if(!pending||unit->frameType!=FRAME_TYPE_IDR)return;
    int index=pending-1;
    // Exclude IDRs already in flight or queued when the request was made.
    if(idr_probes[index].arrival_us||unit->receiveTimeUs<idr_probes[index].request_us)return;
    idr_probes[index].arrival_us=unit->enqueueTimeUs;
    idr_probes[index].wire_frame=unit->frameNumber;
}
static void idr_probe_decoded(int64_t wire_frame,uint64_t decoded_us){
    int pending=atomic_load(&idr_probe_pending);
    if(!pending)return;
    int index=pending-1;
    if(!idr_probes[index].arrival_us||wire_frame!=idr_probes[index].wire_frame)return;
    idr_probes[index].decoded_us=decoded_us;
    atomic_store(&idr_probe_pending,0);
}
static int summarize_idr_probe(FILE *output,FILE *csv){
    if(!idr_probe_count)return 1;
    double values[MAX_IDR_PROBES],sum=0;unsigned count=0,decoded=0;
    fprintf(csv,"sample,request_ms,arrival_ms,decoded_ms,request_to_arrival_ms,request_to_decoded_ms,wire_frame,status\n");
    for(int i=0;i<idr_probe_count;i++){
        uint64_t request=idr_probes[i].request_us,arrival=idr_probes[i].arrival_us,done=idr_probes[i].decoded_us;
        double arrival_ms=arrival?(arrival-request)/1000.0:-1,decoded_ms=done?(done-request)/1000.0:-1;
        if(arrival){values[count++]=arrival_ms;sum+=arrival_ms;}
        if(done)decoded++;
        fprintf(csv,"%d,%.3f,%.3f,%.3f,%.3f,%.3f,%d,%s\n",i+1,
            i<idr_probe_sent?request/1000.0:-1,arrival?arrival/1000.0:-1,done?done/1000.0:-1,
            arrival_ms,decoded_ms,arrival?idr_probes[i].wire_frame:-1,
            i>=idr_probe_sent?"not_requested":!arrival?"no_idr":!done?"not_decoded":"ok");
    }
    qsort(values,count,sizeof(double),compare_double);
    fprintf(output,"IDR_PROBE samples=%u mean_ms=%.3f p50_ms=%.3f p95_ms=%.3f max_ms=%.3f requested=%d sent=%d decoded=%u\n",
        count,count?sum/count:NAN,count?values[(count-1)/2]:NAN,count?values[(count-1)*95/100]:NAN,
        count?values[count-1]:NAN,idr_probe_count,idr_probe_sent,decoded);
    return count==(unsigned)idr_probe_count&&decoded==(unsigned)idr_probe_count;
}
static unsigned luma_sample(const AVFrame *frame,int x,int y){
    const AVPixFmtDescriptor *desc=av_pix_fmt_desc_get(frame->format);
    if(!desc||desc->comp[0].plane!=0||x>=frame->width||y>=frame->height)return 0;
    const AVComponentDescriptor *component=&desc->comp[0];
    const unsigned char *pixel=frame->data[0]+y*frame->linesize[0]+x*component->step+component->offset;
    unsigned value=pixel[0];if(component->depth>8)value|=(unsigned)pixel[1]<<8;
    return(value>>component->shift)&((1u<<component->depth)-1u);
}
static enum AVPixelFormat select_hardware_format(AVCodecContext *context,const enum AVPixelFormat *formats){
    (void)context;
    for(const enum AVPixelFormat *format=formats;*format!=AV_PIX_FMT_NONE;format++)if(*format==hardware_format)return *format;
    fprintf(stderr,"Requested hardware pixel format is unavailable\n");return AV_PIX_FMT_NONE;
}
static int barcode_sample_y(int height,int row){
    return barcode_bottom?height-(int)(barcode_bottom_margin+(128-20-row*24)*barcode_scale):(int)((20+row*24)*barcode_scale);
}
static int picture_timestamp(const AVFrame *frame,const AVFrame *strip,int strip_left,uint32_t *sequence,uint64_t *ticks){
    const double s=barcode_scale;
    if(frame->width<640*s||frame->height<128*s)return 0;
    const AVFrame *samples=strip?strip:frame;
    int left=strip?strip_left:0;
    if(strip&&((int)(barcode_left+12*s)<left||(int)(barcode_left+568*s)>=left+strip->width))return 0;
    uint32_t words[4]={0};
    for(int row=0;row<4;row++){
        int y=barcode_sample_y(frame->height,row);
        if(strip){if(y<0||y>=frame->height)return 0;y=row*2+(y&1);}
        unsigned black=luma_sample(samples,(int)(barcode_left+12*s)-left,y),white=luma_sample(samples,(int)(barcode_left+36*s)-left,y);
        if(white<=black+32)return 0;
        unsigned threshold=(black+white)/2;
        for(int bit=0;bit<32;bit++)if(luma_sample(samples,(int)(barcode_left+(72+bit*16)*s)-left,y)>threshold)words[row]|=1u<<bit;
    }
    if(words[3]!=0xB17E2212||words[0]==0)return 0;
    *sequence=words[0];*ticks=((uint64_t)words[2]<<32)|words[1];return 1;
}
static void crop_picture(AVFrame *frame){
    // AMD AV1 surfaces can include alignment padding outside the stream.
    // Crop before sampling pixels, the bottom barcode or a picture dump.
    if((requested_format&VIDEO_FORMAT_MASK_AV1)&&
       frame->width>=requested_width&&frame->width<=requested_width+64&&
       frame->height>=requested_height&&frame->height<=requested_height+16){
        frame->width=requested_width;frame->height=requested_height;
    }
}
#ifdef _WIN32
static void video_cleanup(void){
    if(barcode_staging){ID3D11Texture2D_Release(barcode_staging);barcode_staging=NULL;}
}
static int readback_barcode(const AVFrame *decoded,AVFrame *frame,AVFrame *strip,int *left){
    AVHWFramesContext *frames_context=(AVHWFramesContext*)decoded->hw_frames_ctx->data;
    AVD3D11VADeviceContext *device=frames_context->device_ctx->hwctx;
    ID3D11Texture2D *texture=(ID3D11Texture2D*)decoded->data[0];
    D3D11_TEXTURE2D_DESC source,staging;
    ID3D11Texture2D_GetDesc(texture,&source);
    if(source.Format!=DXGI_FORMAT_NV12&&source.Format!=DXGI_FORMAT_P010){
        fprintf(stderr,"Unsupported D3D11 barcode texture format: %d\n",source.Format);return -1;
    }
    frame->width=decoded->width;frame->height=decoded->height;frame->format=frames_context->sw_format;
    if(av_frame_copy_props(frame,decoded)<0)return -1;
    crop_picture(frame);
    // Planar 4:2:0 copies need even coordinates. Pack one two-row band per
    // barcode sample, retaining only the scaled barcode's horizontal extent.
    *left=(int)fmax(0,fmin(floor(barcode_left),frame->width-2))&~1;
    int right=((int)fmax(*left+2,fmin(ceil(barcode_left+640*barcode_scale),frame->width))+1)&~1;
    strip->width=right-*left;strip->height=8;strip->format=frame->format;
    if(barcode_staging){
        ID3D11Texture2D_GetDesc(barcode_staging,&staging);
        if(staging.Width!=(UINT)strip->width||staging.Format!=source.Format)video_cleanup();
    }
    if(!barcode_staging){
        staging=(D3D11_TEXTURE2D_DESC){.Width=strip->width,.Height=8,.MipLevels=1,.ArraySize=1,
            .Format=source.Format,.SampleDesc={.Count=1},.Usage=D3D11_USAGE_STAGING,.CPUAccessFlags=D3D11_CPU_ACCESS_READ};
        HRESULT result=ID3D11Device_CreateTexture2D(device->device,&staging,NULL,&barcode_staging);
        if(FAILED(result)){fprintf(stderr,"D3D11 barcode staging texture failed: 0x%08lx\n",(unsigned long)result);return -1;}
        printf("HARDWARE_READBACK strip=%dx%d format=%s\n",strip->width,strip->height,av_get_pix_fmt_name(strip->format));
    }
    device->lock(device->lock_ctx);
    for(int row=0;row<4;row++){
        int top=av_clip(barcode_sample_y(frame->height,row),0,frame->height-1)&~1;
        D3D11_BOX box={.left=*left,.top=top,.front=0,.right=right,.bottom=top+2,.back=1};
        ID3D11DeviceContext_CopySubresourceRegion(device->device_context,(ID3D11Resource*)barcode_staging,0,0,row*2,0,
            (ID3D11Resource*)texture,(UINT)(uintptr_t)decoded->data[1]*source.MipLevels,&box);
    }
    D3D11_MAPPED_SUBRESOURCE mapped;
    HRESULT result=ID3D11DeviceContext_Map(device->device_context,(ID3D11Resource*)barcode_staging,0,D3D11_MAP_READ,0,&mapped);
    device->unlock(device->lock_ctx);
    if(FAILED(result)){fprintf(stderr,"D3D11 barcode readback failed: 0x%08lx\n",(unsigned long)result);return -1;}
    strip->data[0]=mapped.pData;strip->linesize[0]=mapped.RowPitch;
    return 0;
}
static void unmap_barcode(const AVFrame *decoded){
    AVHWFramesContext *frames_context=(AVHWFramesContext*)decoded->hw_frames_ctx->data;
    AVD3D11VADeviceContext *device=frames_context->device_ctx->hwctx;
    device->lock(device->lock_ctx);
    ID3D11DeviceContext_Unmap(device->device_context,(ID3D11Resource*)barcode_staging,0);
    device->unlock(device->lock_ctx);
}
#endif
static void distribution(const char *name,double *values,unsigned count){
    if(!count)return;
    double sum=0;for(unsigned i=0;i<count;i++)sum+=values[i];
    qsort(values,count,sizeof(double),compare_double);
    printf("%s samples=%u mean_ms=%.3f p50_ms=%.3f p95_ms=%.3f p99_ms=%.3f max_ms=%.3f\n",name,count,sum/count,values[(count-1)/2],values[(count-1)*95/100],values[(count-1)*99/100],values[count-1]);
}
#ifdef BUTTERPOLLO_PYROWAVE
static uint32_t record_word(const unsigned char *p){return (uint32_t)p[0]|((uint32_t)p[1]<<8)|((uint32_t)p[2]<<16)|((uint32_t)p[3]<<24);}
static int decode_pyrowave(const AVPacket *packet,PDECODE_UNIT unit,AVFrame *frame){
    const unsigned char *data=packet->data;size_t length=packet->size;
    if(length<8||length%4)return -1;
    uint32_t first=record_word(data),second=record_word(data+4);
    int chroma=(requested_format&VIDEO_FORMAT_MASK_YUV444)!=0;
    if(first==UINT32_MAX||!(first&0x80000000)||((second>>24)&3)||
       (int)(first&0x3fff)+1!=requested_width||(int)((first>>14)&0x3fff)+1!=requested_height||
       ((second>>26)&1)!=(unsigned)chroma||unit->hdrActive!=requested_hdr)return -1;
    pyrowave_decoder_clear(pyro_decoder);
    unsigned records=0;size_t offset=0;
    while(offset<length){
        if(length-offset<8)return -1;
        uint32_t header=record_word(data+offset);
        size_t size=header==UINT32_MAX?8+(size_t)record_word(data+offset+4)*4:offset==0?8:((header>>16)&0xfff)*4;
        if(size<8||size>length-offset)return -1;
        if(header!=UINT32_MAX){
            if(offset&&((header&0x80000000)||((header>>28)&7)!=((first>>28)&7)))return -1;
            if(pyrowave_decoder_push_packet(pyro_decoder,data+offset,size))return -1;
            if(offset)records++;
        }
        offset+=size;
    }
    if(records!=(second&0xffffff)||!pyrowave_decoder_decode_is_ready(pyro_decoder,false))return -1;
    frame->width=requested_width;frame->height=requested_height;
    frame->format=chroma?AV_PIX_FMT_YUV444P:AV_PIX_FMT_YUV420P;
    if(av_frame_get_buffer(frame,32)<0)return -1;
    pyrowave_cpu_buffer output={.width=frame->width,.height=frame->height,
        .format=chroma?PYROWAVE_CPU_BUFFER_FORMAT_YUV444P:PYROWAVE_CPU_BUFFER_FORMAT_YUV420P};
    for(int plane=0;plane<3;plane++){
        output.data[plane]=frame->data[plane];output.row_stride_in_bytes[plane]=frame->linesize[plane];
        output.plane_size_in_bytes[plane]=(size_t)frame->linesize[plane]*(frame->height/(plane&&!chroma?2:1));
    }
    if(pyrowave_decoder_decode_cpu_buffer_synchronous(pyro_decoder,&output))return -1;
    record_frames++;if(unit->hdrActive)hdr_frames++;
    return 0;
}
#endif
static int video_setup(int format,int width,int height,int rate,void*context,int flags){
    printf("VIDEO format=%d %dx%d@%d\n",format,width,height,rate);
    if(format != requested_format){fprintf(stderr,"Codec fallback: requested=%d negotiated=%d\n",requested_format,format);return -1;}
#ifdef BUTTERPOLLO_PYROWAVE
    if(format&VIDEO_FORMAT_MASK_PYROWAVE){
        if(width!=requested_width||height!=requested_height||pyrowave_create_device_by_compat(0,0,NULL,NULL,NULL,&pyro_device))return -1;
        pyrowave_decoder_create_info info={.device=pyro_device,.width=width,.height=height,
            .chroma=(format&VIDEO_FORMAT_MASK_YUV444)!=0,.fragment_path=false};
        printf("DECODER codec=pyrowave bitstream=%s readback_bits=8\n",PYROWAVE_BITSTREAM_ID);
        return pyrowave_decoder_create(&info,&pyro_decoder);
    }
#endif
    enum AVCodecID id=(format&VIDEO_FORMAT_MASK_H264)?AV_CODEC_ID_H264:(format&VIDEO_FORMAT_MASK_H265)?AV_CODEC_ID_HEVC:AV_CODEC_ID_AV1;
    // libdav1d, FFmpeg's default AV1 decoder, has no hardware path.
    const AVCodec *codec=(getenv("BUTTERPOLLO_TEST_HW_DECODER")&&id==AV_CODEC_ID_AV1)?avcodec_find_decoder_by_name("av1"):NULL;
    if(!codec)codec=avcodec_find_decoder(id);if(!codec){fprintf(stderr,"Independent decoder unavailable for codec %d\n",id);return -1;}
    decoder=avcodec_alloc_context3(codec);if(!decoder)return -1;
    const char *hardware=getenv("BUTTERPOLLO_TEST_HW_DECODER");
    if(hardware){
        enum AVHWDeviceType type=av_hwdevice_find_type_by_name(hardware);
        if(type==AV_HWDEVICE_TYPE_NONE)return -1;
        for(int i=0;;i++){
            const AVCodecHWConfig *config=avcodec_get_hw_config(codec,i);
            if(!config){fprintf(stderr,"Decoder does not support %s\n",hardware);return -1;}
            if(config->device_type==type&&(config->methods&AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX)){hardware_format=config->pix_fmt;break;}
        }
        if(av_hwdevice_ctx_create(&decoder->hw_device_ctx,type,getenv("BUTTERPOLLO_TEST_HW_DEVICE"),NULL,0)<0)return -1;
        decoder->get_format=select_hardware_format;decoder->extra_hw_frames=8;
        printf("HARDWARE_DECODER type=%s format=%s\n",hardware,av_get_pix_fmt_name(hardware_format));
    }
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
    if(idr_probe_count)idr_probe_arrived(unit);
    // Preserve arrival's local-clock epoch without including the decoder queue.
    double arrival_ms=hardware_format!=AV_PIX_FMT_NONE?decode_started-(LiGetMicroseconds()/1000.0-unit->enqueueTimeUs/1000.0):decode_started;
    uint32_t picture_sequence=0;uint64_t picture_ticks=0;double age_ms=-1;
    if(unit->fullLength<1||unit->fullLength>32*1024*1024){atomic_fetch_add(&frames,1);atomic_fetch_add(&failures,1);return DR_OK;}
    AVPacket *packet=av_packet_alloc();AVFrame *frame=av_frame_alloc(),*decoded=av_frame_alloc();
    if(!packet||!frame||!decoded||av_new_packet(packet,unit->fullLength)<0){
        av_packet_free(&packet);av_frame_free(&frame);av_frame_free(&decoded);
        atomic_fetch_add(&frames,1);atomic_fetch_add(&failures,1);return DR_OK;
    }
    if(idr_probe_count)packet->pts=unit->frameNumber;
    int offset=0,complete=1;for(PLENTRY entry=unit->bufferList;entry;entry=entry->next){
#ifdef BUTTERPOLLO_PYROWAVE
        if(entry->bufferType==BUFFER_TYPE_LOST){partial_frames++;complete=0;break;}
#endif
        if(entry->length<=0||entry->length>unit->fullLength-offset){complete=0;break;}
        memcpy(packet->data+offset,entry->data,entry->length);offset+=entry->length;
    }
    if(offset!=unit->fullLength)complete=0;
    // Optional first access-unit dump runs before the steady measurement window.
    // It allows an independent bitstream parser to diagnose driver geometry.
    // Moonlight removes H.264/HEVC AUD and prefix SEI before this callback;
    // missing HDR SEI here does not mean it was absent from the host bitstream.
    const char *dump=getenv("BUTTERPOLLO_TEST_FIRST_FRAME");
    if(dump&&atomic_load(&frames)==0){FILE *file=fopen(dump,"wb");if(!file||fwrite(packet->data,1,packet->size,file)!=(size_t)packet->size)atomic_fetch_add(&failures,1);if(file)fclose(file);}
    // Every access unit as received, appended as an elementary stream, for a
    // header trace (ffmpeg -bsf:v trace_headers) of a recovery the decoder rejects.
    static FILE *bitstream;
    if(!bitstream&&getenv("BUTTERPOLLO_TEST_BITSTREAM"))bitstream=fopen(getenv("BUTTERPOLLO_TEST_BITSTREAM"),"wb");
    if(bitstream&&complete){fwrite(packet->data,1,packet->size,bitstream);fflush(bitstream);}
    int received=AVERROR(EAGAIN);
#ifdef BUTTERPOLLO_PYROWAVE
    if(pyro_decoder){
        if(!complete||decode_pyrowave(packet,unit,decoded)<0)atomic_fetch_add(&failures,1);
        else{decoded->pts=packet->pts;received=0;}
    }else
#endif
    {
        if(!complete||avcodec_send_packet(decoder,packet)<0)atomic_fetch_add(&failures,1);
        received=avcodec_receive_frame(decoder,decoded);
    }
    while(received==0){
        const char *picture_dump=getenv("BUTTERPOLLO_TEST_FRAME_DUMP");
        int dump_at=getenv("BUTTERPOLLO_TEST_FRAME_DUMP_AT")?atoi(getenv("BUTTERPOLLO_TEST_FRAME_DUMP_AT")):600;
        AVFrame strip={0};int strip_left=0;
        // Read hardware surfaces back before validating pixels, geometry and
        // HDR. Decoder timing includes this readback, but excludes scanout.
        if(hardware_format!=AV_PIX_FMT_NONE){
            int result=-1;
            if(decoded->format==hardware_format){
#ifdef _WIN32
                int full_picture=(dump&&atomic_load(&frames)==0)||(picture_dump&&atomic_load(&decoded_frames)+1==dump_at);
                if(decoded->format==AV_PIX_FMT_D3D11&&!full_picture)result=readback_barcode(decoded,frame,&strip,&strip_left);
                else
#endif
                if(av_hwframe_transfer_data(frame,decoded,0)>=0)result=av_frame_copy_props(frame,decoded);
            }
            if(result<0){
                atomic_fetch_add(&failures,1);av_frame_unref(decoded);av_frame_unref(frame);
                received=avcodec_receive_frame(decoder,decoded);continue;
            }
        }else av_frame_move_ref(frame,decoded);
        // Readback above waits for hardware decoding; PTS also matches delayed output.
        if(idr_probe_count)idr_probe_decoded(frame->pts,LiGetMicroseconds());
        crop_picture(frame);
        if(frame->width!=requested_width||frame->height!=requested_height)atomic_fetch_add(&failures,1);
        const AVFrame *samples=strip.data[0]?&strip:frame;
        unsigned low=65535,high=0;
        for(int y=1;y<8;y++)for(int x=1;x<12;x++){
            unsigned value=luma_sample(samples,samples->width*x/12,samples->height*y/8);
            if(value<low)low=value;if(value>high)high=value;
        }
        const AVPixFmtDescriptor *pixel=av_pix_fmt_desc_get(frame->format);
        if(pixel&&high>low+(16u<<(pixel->comp[0].depth>8?pixel->comp[0].depth-8:0)))atomic_fetch_add(&detailed_frames,1);
        /* The SDK's CPU readback is eight-bit even for HDR. Its stream mode is
         * checked above; full-precision HDR quality has a separate GPU test. */
#ifdef BUTTERPOLLO_PYROWAVE
        if(!pyro_decoder)
#endif
        if(requested_hdr){const AVPixFmtDescriptor *desc=av_pix_fmt_desc_get(frame->format);if(!desc||desc->comp[0].depth<10||frame->color_primaries!=AVCOL_PRI_BT2020||frame->color_trc!=AVCOL_TRC_SMPTE2084)atomic_fetch_add(&failures,1);}
        atomic_fetch_add(&decoded_frames,1);if(atomic_load(&decoded_frames)==1)printf("DECODED %dx%d pixel_format=%d primaries=%d transfer=%d\n",frame->width,frame->height,frame->format,frame->color_primaries,frame->color_trc);
        // Optional decoded picture for colour comparisons between hosts:
        // a text header, then each plane's rows as decoded.
        if(picture_dump&&atomic_load(&decoded_frames)==dump_at){
            const AVPixFmtDescriptor *desc=av_pix_fmt_desc_get(frame->format);FILE *file=fopen(picture_dump,"wb");
            if(file&&desc){
                int bytes=(desc->comp[0].depth+7)/8;
                fprintf(file,"BPFRAME %d %d %s %d %d %d %d\n",frame->width,frame->height,desc->name,desc->comp[0].depth,frame->color_range,frame->colorspace,desc->nb_components);
                for(int plane=0;plane<3&&frame->data[plane];plane++){
                    int w=plane?AV_CEIL_RSHIFT(frame->width,desc->log2_chroma_w):frame->width,h=plane?AV_CEIL_RSHIFT(frame->height,desc->log2_chroma_h):frame->height;
                    int step=(desc->flags&AV_PIX_FMT_FLAG_PLANAR)||plane==0?1:2;
                    for(int y=0;y<h;y++)fwrite(frame->data[plane]+(size_t)y*frame->linesize[plane],1,(size_t)w*bytes*step,file);
                }
            }
            if(file)fclose(file);
            printf("FRAME_DUMP frame=%d format=%s\n",dump_at,desc?desc->name:"unknown");
        }
        if(picture_timestamp(frame,strip.data[0]?&strip:NULL,strip_left,&picture_sequence,&picture_ticks)){
            // The sequence validates fresh motion on a remote receiver too.
            // Absolute picture age needs the renderer's clock: only compare
            // QPC timestamps on the same Windows machine.
#ifdef _WIN32
            const char *host=getenv("BUTTERPOLLO_TEST_HOST");
            if(!host||strcmp(host,"127.0.0.1")==0||strcmp(host,"::1")==0){
            LARGE_INTEGER frequency;QueryPerformanceFrequency(&frequency);
            age_ms=clock_ms()-(double)picture_ticks*1000.0/(double)frequency.QuadPart;
            // A signature alone must not turn unrelated desktop content into a
            // latency sample. Both processes use the same Windows QPC clock.
            if(age_ms<0||age_ms>3000){picture_sequence=0;age_ms=-1;}
            }
#endif
        }
#ifdef _WIN32
        if(strip.data[0])unmap_barcode(decoded);
#endif
        av_frame_unref(decoded);
        av_frame_unref(frame);
#ifdef BUTTERPOLLO_PYROWAVE
        if(pyro_decoder)received=AVERROR(EAGAIN);else
#endif
        received=avcodec_receive_frame(decoder,decoded);
    }
    if(received!=AVERROR(EAGAIN)&&received!=AVERROR_EOF)atomic_fetch_add(&failures,1);
    av_packet_free(&packet);av_frame_free(&frame);av_frame_free(&decoded);
    double decode_ms=clock_ms()-decode_started;
    if(measured_frames<MAX_MEASUREMENTS){
        host_latency[measured_frames]=unit->frameHostProcessingLatency/10.0;decode_time_ms[measured_frames]=decode_ms;
        arrivals[measured_frames]=arrival_ms;assembly_times[measured_frames]=unit->enqueueTimeUs/1000.0;picture_age[measured_frames]=age_ms;picture_frames[measured_frames]=picture_sequence;measured_frames++;
    }
    if(timing_csv)fprintf(timing_csv,"%d,%.6f,%.3f,%.6f,%u,%llu,%.6f,%llu,%llu,%llu,%d,%d\n",unit->frameNumber,arrival_ms,unit->frameHostProcessingLatency/10.0,decode_ms,picture_sequence,(unsigned long long)picture_ticks,age_ms,(unsigned long long)unit->receiveTimeUs,(unsigned long long)unit->enqueueTimeUs,(unsigned long long)unit->presentationTimeUs,unit->frameType,unit->fullLength);
    atomic_fetch_add(&frames,1);if(atomic_load(&frames)<4)printf("FRAME %d bytes=%d type=%d\n",unit->frameNumber,unit->fullLength,unit->frameType);return DR_OK;
}
static int audio_init(int config,const POPUS_MULTISTREAM_CONFIGURATION opus,void*context,int flags){
    printf("AUDIO channels=%d samples=%d\n",opus->channelCount,opus->samplesPerFrame);
    int error;audio_channels=opus->channelCount;opus_decoder=opus_multistream_decoder_create(48000,audio_channels,opus->streams,opus->coupledStreams,opus->mapping,&error);return error;
}
static void audio_frame(char*data,int size){
    float samples[5760*8];int count=opus_multistream_decode_float(opus_decoder,(unsigned char*)data,size,samples,5760,0);
    double minimum=INFINITY,maximum=0;
    if(count>0){
        atomic_fetch_add(&audio_packets,1);
        if(getenv("BUTTERPOLLO_TEST_AUDIO_TONE")&&audio_samples>=48000u*2*audio_channels){
            /* A continuous tone must stay present after decoder startup. Small
             * windows catch inserted silence even across Opus packet boundaries. */
            for(int start=0;start+120<=count;start+=120){
                double energy=0;
                for(int i=start*audio_channels;i<(start+120)*audio_channels;i++)energy+=(double)samples[i]*samples[i];
                energy/=120*audio_channels;
                minimum=fmin(minimum,energy);maximum=fmax(maximum,energy);
                audio_tone_min_energy=fmin(audio_tone_min_energy,energy);
                audio_tone_max_energy=fmax(audio_tone_max_energy,energy);audio_tone_blocks++;
            }
        }
        for(int i=0;i<count*audio_channels;i++){double v=samples[i];audio_energy+=v*v;if(fabs(v)>audio_peak)audio_peak=fabs(v);audio_samples++;}
    }else atomic_fetch_add(&failures,1);
    if(audio_csv)fprintf(audio_csv,"%.6f,%d,%.9f,%.9f\n",clock_ms(),count,isfinite(minimum)?sqrt(minimum):-1,sqrt(maximum));
}
static void stage_start(int stage){printf("STAGE %s\n",LiGetStageName(stage));}
static void stage_failed(int stage,int error){printf("FAILED %s error=%d\n",LiGetStageName(stage),error);}
static void terminated(int error){printf("TERMINATED error=%d\n",error);atomic_store(&ended,1);}
static void log_message(const char*fmt,...){va_list args;va_start(args,fmt);vprintf(fmt,args);va_end(args);}
static int hdr_chromaticity_valid(unsigned x,unsigned y){
    return y>0&&x<=50000&&y<=50000&&x+y<=50000;
}
static int hdr_metadata_valid(const SS_HDR_METADATA *metadata){
    if(!metadata->maxDisplayLuminance||
       !hdr_chromaticity_valid(metadata->whitePoint.x,metadata->whitePoint.y)||
       (unsigned)metadata->minDisplayLuminance>(unsigned)metadata->maxDisplayLuminance*10000u||
       metadata->maxFullFrameLuminance>metadata->maxDisplayLuminance)return 0;
    for(int i=0;i<3;i++)if(!hdr_chromaticity_valid(metadata->displayPrimaries[i].x,metadata->displayPrimaries[i].y))return 0;
    /* Zero minimum, MaxCLL, MaxFALL and full-frame luminance are valid.
     * The content/display maxima may be unknown, so no fabricated value is required. */
    return 1;
}
static void hdr_mode(bool enabled){
    SS_HDR_METADATA metadata={0};
    int available=LiGetHdrMetadata(&metadata);
    int valid=available&&hdr_metadata_valid(&metadata);
    int notification=atomic_fetch_add(&hdr_notifications,1)+1;
    if(enabled)atomic_fetch_add(&hdr_enabled_notifications,1);
    else atomic_fetch_add(&hdr_disabled_notifications,1);
    if(expected_hdr_control>=0&&(int)enabled!=expected_hdr_control)atomic_fetch_add(&hdr_control_mismatches,1);
    if(enabled&&!valid)atomic_fetch_add(&hdr_invalid_metadata,1);
    /* One record per callback; the callback owns the metadata snapshot and
     * cross-thread summary state is atomic. Values retain Moonlight's wire units. */
    if(available){
        printf("HDR_CONTROL {\"notification\":%d,\"enabled\":%s,\"metadata_available\":true,\"metadata_valid\":%s,"
               "\"primaries_xy_50000\":[[%u,%u],[%u,%u],[%u,%u]],\"white_xy_50000\":[%u,%u],"
               "\"maximum_nits\":%u,\"minimum_10000th_nit\":%u,\"max_cll_nits\":%u,\"max_fall_nits\":%u,\"full_frame_nits\":%u}\n",
               notification,enabled?"true":"false",valid?"true":"false",
               (unsigned)metadata.displayPrimaries[0].x,(unsigned)metadata.displayPrimaries[0].y,
               (unsigned)metadata.displayPrimaries[1].x,(unsigned)metadata.displayPrimaries[1].y,
               (unsigned)metadata.displayPrimaries[2].x,(unsigned)metadata.displayPrimaries[2].y,
               (unsigned)metadata.whitePoint.x,(unsigned)metadata.whitePoint.y,
               (unsigned)metadata.maxDisplayLuminance,(unsigned)metadata.minDisplayLuminance,
               (unsigned)metadata.maxContentLightLevel,(unsigned)metadata.maxFrameAverageLightLevel,
               (unsigned)metadata.maxFullFrameLuminance);
    }else printf("HDR_CONTROL {\"notification\":%d,\"enabled\":%s,\"metadata_available\":false,\"metadata_valid\":false}\n",notification,enabled?"true":"false");
}
int main(int argc,char**argv){
    if(argc<2){fprintf(stderr,"session URL required\n");return 2;}
    setbuf(stdout,NULL);
    SERVER_INFORMATION server;LiInitializeServerInformation(&server);
    server.address=getenv("BUTTERPOLLO_TEST_HOST")?getenv("BUTTERPOLLO_TEST_HOST"):"127.0.0.1";server.serverInfoAppVersion="7.1.431.-1";server.serverInfoGfeVersion="3.23.0.74";server.rtspSessionUrl=argv[1];server.serverCodecModeSupport=0x30301;
    STREAM_CONFIGURATION config;LiInitializeStreamConfiguration(&config);config.width=640;config.height=480;config.fps=30;config.bitrate=2000;config.packetSize=1024;config.streamingRemotely=STREAM_CFG_LOCAL;config.audioConfiguration=AUDIO_CONFIGURATION_STEREO;config.supportedVideoFormats=VIDEO_FORMAT_H264;config.encryptionFlags=ENCFLG_ALL;
    if(argc>2){if(strcmp(argv[2],"hevc")==0)config.supportedVideoFormats=VIDEO_FORMAT_H265;else if(strcmp(argv[2],"hevc-hdr")==0)config.supportedVideoFormats=VIDEO_FORMAT_H265_MAIN10;else if(strcmp(argv[2],"av1")==0)config.supportedVideoFormats=VIDEO_FORMAT_AV1_MAIN8;else if(strcmp(argv[2],"av1-hdr")==0)config.supportedVideoFormats=VIDEO_FORMAT_AV1_MAIN10;}
#ifdef BUTTERPOLLO_PYROWAVE
    if(argc>2){
        if(strcmp(argv[2],"pyrowave")==0)config.supportedVideoFormats=VIDEO_FORMAT_PYROWAVE;
        else if(strcmp(argv[2],"pyrowave-444")==0)config.supportedVideoFormats=VIDEO_FORMAT_PYROWAVE_444;
        else if(strcmp(argv[2],"pyrowave-hdr")==0)config.supportedVideoFormats=VIDEO_FORMAT_PYROWAVE_HDR10;
        else if(strcmp(argv[2],"pyrowave-hdr-444")==0)config.supportedVideoFormats=VIDEO_FORMAT_PYROWAVE_HDR10_444;
    }
    server.serverCodecModeSupport|=SCM_MASK_PYROWAVE;config.packetSize=1392;
#endif
    if(argc>2&&strcmp(argv[2],"h264")!=0&&config.supportedVideoFormats==VIDEO_FORMAT_H264){fprintf(stderr,"Unknown codec: %s\n",argv[2]);return 2;}
    requested_format=config.supportedVideoFormats;requested_hdr=(requested_format&(VIDEO_FORMAT_H265_MAIN10|VIDEO_FORMAT_AV1_MAIN10))!=0;
#ifdef BUTTERPOLLO_PYROWAVE
    if(requested_format&VIDEO_FORMAT_MASK_PYROWAVE)requested_hdr=(requested_format&VIDEO_FORMAT_MASK_10BIT)!=0;
#endif
    const char *hdr_expectation=getenv("BUTTERPOLLO_TEST_EXPECT_HDR_CONTROL");
    if(hdr_expectation){
        if(strcmp(hdr_expectation,"auto")==0)expected_hdr_control=requested_hdr;
        else if(strcmp(hdr_expectation,"0")==0)expected_hdr_control=0;
        else if(strcmp(hdr_expectation,"1")==0)expected_hdr_control=1;
        else{fprintf(stderr,"BUTTERPOLLO_TEST_EXPECT_HDR_CONTROL must be auto, 0 or 1\n");return 2;}
    }
    int duration=argc>6?atoi(argv[6]):0;
    if(getenv("BUTTERPOLLO_TEST_WARMUP_SECONDS"))warmup_seconds=atof(getenv("BUTTERPOLLO_TEST_WARMUP_SECONDS"));
    idr_probe_count=parse_idr_probe(getenv("BUTTERPOLLO_TEST_IDR_PROBE"));
    if(idr_probe_count<0){fprintf(stderr,"BUTTERPOLLO_TEST_IDR_PROBE must be an integer from 1 to %d\n",MAX_IDR_PROBES);return 2;}
    long idr_probe_interval_ms=parse_idr_probe_interval_ms(getenv("BUTTERPOLLO_TEST_IDR_PROBE_INTERVAL_MS"));
    if(idr_probe_interval_ms<0){fprintf(stderr,"BUTTERPOLLO_TEST_IDR_PROBE_INTERVAL_MS must be an integer from 100 to 10000\n");return 2;}
    idr_probe_interval_us=(uint64_t)idr_probe_interval_ms*1000;
    if(idr_probe_count&&(!isfinite(warmup_seconds)||duration<warmup_seconds+idr_probe_count*idr_probe_interval_ms/1000.0)){
        fprintf(stderr,"IDR probe requires an explicit duration of at least warmup + N request intervals\n");return 2;
    }
    barcode_bottom=getenv("BUTTERPOLLO_TEST_BARCODE_BOTTOM")&&strcmp(getenv("BUTTERPOLLO_TEST_BARCODE_BOTTOM"),"1")==0;
    if(getenv("BUTTERPOLLO_TEST_BARCODE_SCALE"))barcode_scale=atof(getenv("BUTTERPOLLO_TEST_BARCODE_SCALE"));
    if(getenv("BUTTERPOLLO_TEST_BARCODE_LEFT"))barcode_left=atof(getenv("BUTTERPOLLO_TEST_BARCODE_LEFT"));
    if(getenv("BUTTERPOLLO_TEST_BARCODE_BOTTOM_MARGIN"))barcode_bottom_margin=atof(getenv("BUTTERPOLLO_TEST_BARCODE_BOTTOM_MARGIN"));
    if(!(barcode_scale>0.0&&barcode_scale<=1.0))barcode_scale=1.0;
    if(warmup_seconds<0||warmup_seconds>60)return 2;
    if(idr_probe_count){
        const char *timing_path=getenv("BUTTERPOLLO_TEST_TIMING_CSV");
        size_t length=(timing_path?strlen(timing_path):0)+32;
        char *path=malloc(length);if(!path)return 2;
        if(timing_path)snprintf(path,length,"%s.idr.csv",timing_path);else snprintf(path,length,"idr-probe.csv");
        idr_csv=fopen(path,"w");free(path);if(!idr_csv){perror("IDR probe CSV");return 2;}
    }
    if(getenv("BUTTERPOLLO_TEST_TIMING_CSV")){
        timing_csv=fopen(getenv("BUTTERPOLLO_TEST_TIMING_CSV"),"w");if(!timing_csv){perror("timing CSV");return 2;}
        fprintf(timing_csv,"wire_frame,arrival_ms,host_ms,decode_ms,render_frame,render_qpc,picture_age_ms,first_packet_us,assembled_us,presentation_us,frame_type,bytes\n");
    }
    if(getenv("BUTTERPOLLO_TEST_AUDIO_CSV")){
        audio_csv=fopen(getenv("BUTTERPOLLO_TEST_AUDIO_CSV"),"w");if(!audio_csv){perror("audio CSV");return 2;}
        fprintf(audio_csv,"arrival_ms,samples,min_rms,max_rms\n");
    }
    if(argc>3)config.width=atoi(argv[3]);if(argc>4)config.height=atoi(argv[4]);if(argc>5)config.fps=atoi(argv[5]);if(argc>7)config.bitrate=atoi(argv[7]);
    if(argc>8)decoder_threads=atoi(argv[8]);requested_width=config.width;requested_height=config.height;
    if(config.width<2||config.width>8192||config.height<2||config.height>8192||config.fps<1||config.fps>240||duration<0||duration>3500||decoder_threads<1||decoder_threads>16)return 2;
    printf("DECODER threads=%d\n",decoder_threads);
    for(int i=0;i<16;i++)config.remoteInputAesKey[i]=(char)i;config.remoteInputAesIv[3]=123;
    /* Concurrent clients need distinct stream keys: rikey byte i is i+seed and rikeyid is 123+seed. */
    if(getenv("BUTTERPOLLO_TEST_KEY_SEED")){int seed=atoi(getenv("BUTTERPOLLO_TEST_KEY_SEED"))&0x7f;for(int i=0;i<16;i++)config.remoteInputAesKey[i]=(char)(i+seed);config.remoteInputAesIv[3]=(char)(123+seed);}
    if(getenv("BUTTERPOLLO_TEST_SIGNED_KEY_ID")&&strcmp(getenv("BUTTERPOLLO_TEST_SIGNED_KEY_ID"),"1")==0)config.remoteInputAesIv[0]=(char)0x80;
    CONNECTION_LISTENER_CALLBACKS listener;LiInitializeConnectionCallbacks(&listener);listener.stageStarting=stage_start;listener.stageFailed=stage_failed;listener.connectionTerminated=terminated;listener.logMessage=log_message;listener.setHdrMode=hdr_mode;
    DECODER_RENDERER_CALLBACKS video;LiInitializeVideoCallbacks(&video);video.setup=video_setup;video.submitDecodeUnit=video_frame;video.capabilities=CAPABILITY_DIRECT_SUBMIT;
    // Moonlight's VideoDec thread drains the decode-unit queue when direct
    // submission is disabled, so GPU waits cannot block the receive thread.
    if(getenv("BUTTERPOLLO_TEST_HW_DECODER")){
        video.capabilities=0;
#ifdef _WIN32
        video.cleanup=video_cleanup;
#endif
    }
#ifdef BUTTERPOLLO_PYROWAVE
    /* SDK decoding waits for GPU readback; keep the UDP receive thread free. */
    if(requested_format&VIDEO_FORMAT_MASK_PYROWAVE)video.capabilities=0;
#endif
    /* Declare reference frame invalidation like Moonlight's hardware decoders, so a lost frame is recovered without a keyframe when the host supports it. */
    if(getenv("BUTTERPOLLO_TEST_RFI")&&strcmp(getenv("BUTTERPOLLO_TEST_RFI"),"1")==0)video.capabilities|=CAPABILITY_REFERENCE_FRAME_INVALIDATION_AVC|CAPABILITY_REFERENCE_FRAME_INVALIDATION_HEVC|CAPABILITY_REFERENCE_FRAME_INVALIDATION_AV1;
    AUDIO_RENDERER_CALLBACKS audio;LiInitializeAudioCallbacks(&audio);audio.init=audio_init;audio.decodeAndPlaySample=audio_frame;audio.capabilities=CAPABILITY_DIRECT_SUBMIT;
    int result=LiStartConnection(&server,&config,&listener,&video,&audio,NULL,0,NULL,0);
    if(result){printf("CONNECT FAILED %d\n",result);return 1;}
#ifdef BUTTERPOLLO_PYROWAVE
    if(pyro_decoder&&(strcmp(LiGetHostPyroWaveBitstreamId(),PYROWAVE_BITSTREAM_ID)||(EncryptionFeaturesEnabled&6)!=6)){
        fprintf(stderr,"PyroWave requires bitstream=%s and encrypted video/audio: bitstream=%s encryption=%u\n",PYROWAVE_BITSTREAM_ID,LiGetHostPyroWaveBitstreamId(),EncryptionFeaturesEnabled);LiStopConnection();return 1;
    }
#endif
    double started=clock_ms();
    uint64_t probe_started=idr_probe_count?LiGetMicroseconds():0;
    while(clock_ms()-started<(duration?duration*1000.0:10000)&&!atomic_load(&ended)&&(duration||atomic_load(&frames)<30)){
        if(idr_probe_count)poll_idr_probe(LiGetMicroseconds(),probe_started);
        wait_ms(100);
    }
    double seconds=(clock_ms()-started)/1000.0;
    int premature=atomic_load(&ended)||(duration&&seconds<duration*0.98);
    LiStopConnection();
    if(idr_probe_count){
        if(!summarize_idr_probe(stdout,idr_csv)){
            fprintf(stderr,"IDR probe incomplete: not every request received and decoded an IDR\n");atomic_fetch_add(&failures,1);
        }
        if(fclose(idr_csv))atomic_fetch_add(&failures,1);
    }
#ifdef BUTTERPOLLO_PYROWAVE
    if(pyro_decoder)printf("PYROWAVE framing=records bitstream=%s encrypted=1 record_frames=%u partial_frames=%u hdr_frames=%u\n",PYROWAVE_BITSTREAM_ID,record_frames,partial_frames,hdr_frames);
#endif
    int hdr_control_valid=expected_hdr_control<0||(atomic_load(&hdr_notifications)>0&&atomic_load(&hdr_control_mismatches)==0&&atomic_load(&hdr_invalid_metadata)==0);
    printf("HDR_CONTROL_RESULT {\"checked\":%s,\"expected_enabled\":%d,\"notifications\":%d,\"enabled_notifications\":%d,\"disabled_notifications\":%d,\"mismatches\":%d,\"invalid_metadata\":%d,\"passed\":%s}\n",
           expected_hdr_control>=0?"true":"false",expected_hdr_control,atomic_load(&hdr_notifications),
           atomic_load(&hdr_enabled_notifications),atomic_load(&hdr_disabled_notifications),
           atomic_load(&hdr_control_mismatches),atomic_load(&hdr_invalid_metadata),hdr_control_valid?"true":"false");
    if(!hdr_control_valid){fprintf(stderr,"HDR control expectation failed: missing/wrong mode notification or invalid HDR metadata\n");atomic_fetch_add(&failures,1);}
    printf("RESULT frames=%d decoded_frames=%d audio_packets=%d failures=%d\n",atomic_load(&frames),atomic_load(&decoded_frames),atomic_load(&audio_packets),atomic_load(&failures));
    printf("PICTURE_CONTENT frames_with_luma_contrast=%d\n",atomic_load(&detailed_frames));
    if(timing_csv)fclose(timing_csv);
    if(audio_csv)fclose(audio_csv);
    printf("AUDIO_SIGNAL samples=%llu peak=%.6f rms=%.6f\n",audio_samples,audio_peak,audio_samples?sqrt(audio_energy/audio_samples):0.0);
    if(getenv("BUTTERPOLLO_TEST_AUDIO_TONE")){
        int continuous=audio_tone_blocks&&audio_tone_min_energy>=audio_tone_max_energy*.25;
        printf("AUDIO_TONE blocks=%u min_rms=%.6f max_rms=%.6f continuous=%d\n",audio_tone_blocks,sqrt(audio_tone_min_energy),sqrt(audio_tone_max_energy),continuous);
        if(!continuous)atomic_fetch_add(&failures,1);
    }
    int motion_valid=!getenv("BUTTERPOLLO_TEST_REQUIRE_MOTION");
    const char *minimum_fps_text=getenv("BUTTERPOLLO_TEST_MIN_FPS");
    double minimum_fps=minimum_fps_text?atof(minimum_fps_text):0;
    int rate_valid=!minimum_fps_text;
    if(duration&&measured_frames){
        double host_sum=0,decode_sum=0;for(unsigned i=0;i<measured_frames;i++){host_sum+=host_latency[i];decode_sum+=decode_time_ms[i];}
        static double steady_host[MAX_MEASUREMENTS],intervals[MAX_MEASUREMENTS],ages[MAX_MEASUREMENTS];
        unsigned steady_count=0,interval_count=0,visual_count=0,age_count=0,repeats=0,skips=0,unique=0,late=0;
        uint32_t last_picture=0;double first_steady=0,last_steady=0;
        for(unsigned i=0;i<measured_frames;i++)if(arrivals[i]>=arrivals[0]+warmup_seconds*1000){
            steady_host[steady_count++]=host_latency[i];
            if(!first_steady)first_steady=arrivals[i];last_steady=arrivals[i];
            if(i){double interval=assembly_times[i]-assembly_times[i-1];intervals[interval_count++]=interval;if(interval>1500.0/config.fps)late++;}
            if(picture_frames[i]){
                visual_count++;if(picture_age[i]>=0)ages[age_count++]=picture_age[i];
                if(picture_frames[i]==last_picture)repeats++;
                else{unique++;if(last_picture&&picture_frames[i]>last_picture+1)skips+=picture_frames[i]-last_picture-1;}
                last_picture=picture_frames[i];
            }
        }
        double steady_seconds=(last_steady-first_steady)/1000;
        double steady_fps=steady_seconds>0&&steady_count>1?(steady_count-1)/steady_seconds:0;
        printf("STEADY warmup_seconds=%.3f seconds=%.3f frames=%u fps=%.3f intervals_over_1_5_period=%u\n",warmup_seconds,steady_seconds,steady_count,steady_fps,late);
        if(minimum_fps_text)rate_valid=isfinite(minimum_fps)&&minimum_fps>0&&steady_fps>=minimum_fps;
        distribution("STEADY_HOST",steady_host,steady_count);distribution("ARRIVAL_INTERVAL",intervals,interval_count);
        if(visual_count){
            printf("VISUAL frames=%u unique=%u repeats=%u skipped_render_frames=%u unique_fps=%.3f coverage=%.6f\n",visual_count,unique,repeats,skips,steady_seconds>0?(unique?unique-1:0)/steady_seconds:0,steady_count?(double)visual_count/steady_count:0);
            distribution("PICTURE_AGE",ages,age_count);
            if(steady_count&&visual_count>=steady_count*0.95&&unique>1)motion_valid=1;
        }
        qsort(host_latency,measured_frames,sizeof(double),compare_double);
        printf("PERFORMANCE seconds=%.3f received_fps=%.2f decoded_fps=%.2f host_mean_ms=%.3f host_p50_ms=%.3f host_p95_ms=%.3f decoder_mean_ms=%.3f\n",seconds,atomic_load(&frames)/seconds,atomic_load(&decoded_frames)/seconds,host_sum/measured_frames,host_latency[(measured_frames-1)/2],host_latency[(measured_frames-1)*95/100],decode_sum/measured_frames);
    }
    if(!motion_valid)fprintf(stderr,"Motion measurement failed: missing timestamps or static test content\n");
    if(!rate_valid)fprintf(stderr,"Stream did not sustain the required %.3f FPS after warmup\n",minimum_fps);
#ifdef BUTTERPOLLO_PYROWAVE
    if(pyro_decoder)pyrowave_decoder_destroy(pyro_decoder);
    if(pyro_device)pyrowave_device_destroy(pyro_device);
#endif
    avcodec_free_context(&decoder);if(opus_decoder)opus_multistream_decoder_destroy(opus_decoder);
    return !premature&&motion_valid&&rate_valid&&(idr_probe_count||atomic_load(&decoded_frames)>=30)&&atomic_load(&audio_packets)>0&&atomic_load(&failures)==0&&(!getenv("BUTTERPOLLO_TEST_AUDIO_TONE")||audio_peak>0.01)&&(!getenv("BUTTERPOLLO_TEST_REQUIRE_PICTURE")||atomic_load(&detailed_frames)>0)?0:1;
}
