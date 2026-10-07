
Texture2D<float4> source : register(t0);
Texture2D<float4> pointer : register(t1);
cbuffer Config : register(b0) {
    uint2 sourceSize; uint2 targetSize;
    uint pixel; uint hdr; uint colorMatrix; uint fullRange;
    uint tenBit; float sdrWhiteScale; float hdrScale; uint padding;
    int2 pointerPosition; uint2 pointerSize;
    uint pointerMode; uint3 pointerPadding;
};
float4 vertex(uint id : SV_VertexID) : SV_Position {
    float2 p = float2((id << 1) & 2, id & 2);
    return float4(p * float2(2, -2) + float2(-1, 1), 0, 1);
}
float3 srgb_linear(float3 rgb) { return lerp(pow((rgb + 0.055) / 1.055, 2.4), rgb / 12.92, step(rgb, 0.04045)); }
float3 gamma(float3 rgb) { return lerp(1.055 * pow(max(rgb, 0), 1.0 / 2.4) - 0.055, rgb * 12.92, step(rgb, 0.0031308)); }
float3 gamut2020(float3 rgb) {
    return float3(dot(rgb, float3(0.627404, 0.329283, 0.043313)),
                  dot(rgb, float3(0.069097, 0.919540, 0.011362)),
                  dot(rgb, float3(0.016391, 0.088013, 0.895595)));
}
float3 pq(float3 luminance) {
    float3 power = pow(clamp(luminance, 0, 1), 2610.0 / 16384.0);
    return pow((3424.0 / 4096.0 + 2413.0 / 128.0 * power) /
               (1.0 + 2392.0 / 128.0 * power), 2523.0 / 32.0);
}
float3 depq(float3 rgb) {
    float3 p = pow(clamp(rgb, 0, 1), 32.0 / 2523.0);
    return pow(max(p - 3424.0 / 4096.0, 0) / max(2413.0 / 128.0 - 2392.0 / 128.0 * p, 0.000001), 16384.0 / 2610.0);
}
float3 pointer_srgb(float3 rgb) {
    if (pixel == 1) return saturate(gamma(rgb / sdrWhiteScale));
    if (pixel == 2) {
        float3 r = depq(rgb) * 125.0 / sdrWhiteScale;
        return saturate(gamma(float3(dot(r, float3(1.660491,-0.587641,-0.072850)),
                                     dot(r, float3(-0.124550,1.132900,-0.008349)),
                                     dot(r, float3(-0.018151,-0.100579,1.118730)))));
    }
    return rgb;
}
float3 pointer_source(float3 rgb) {
    if (pixel == 1) return srgb_linear(rgb) * sdrWhiteScale;
    if (pixel == 2) return pq(gamut2020(srgb_linear(rgb)) * sdrWhiteScale / 125.0);
    return rgb;
}
float3 load(int2 p) {
    p = clamp(p, int2(0, 0), int2(sourceSize) - 1);
    float3 rgb = source.Load(int3(p, 0)).rgb;
    int2 at = p - pointerPosition;
    if (pointerMode != 0 && all(at >= 0) && all(at < int2(pointerSize))) {
        float4 c = pointer.Load(int3(at, 0));
        if (pointerMode == 2 && c.a > 0.25 && c.a < 0.75) {
            uint3 bits = uint3(round(saturate(pointer_srgb(rgb)) * 255)) ^ uint3(round(c.rgb * 255));
            rgb = pointer_source(float3(bits) / 255);
        } else if (pixel == 2) {
            rgb = pq(lerp(depq(rgb), gamut2020(srgb_linear(c.rgb)) * sdrWhiteScale / 125.0, c.a));
        } else {
            rgb = lerp(rgb, pointer_source(c.rgb), c.a);
        }
    }
    if (hdr != 0 && pixel == 0) {
        rgb = srgb_linear(rgb);
        rgb *= sdrWhiteScale;
    }
    if (hdr != 0 && pixel == 1) rgb *= hdrScale;
    // Windows can compose an SDR desktop in FP16 (advanced color); an SDR
    // stream needs it back in sRGB with SDR white at full scale.
    if (hdr == 0 && pixel == 1) rgb = gamma(saturate(rgb / sdrWhiteScale));
    return rgb;
}
// The source keeps its aspect ratio, centred in the stream with black bars
// (a 32:9 desktop streamed to a 16:10 tablet). Sizes within a pixel of the
// stream's shape still fill it, so rounding never adds a one-pixel line.
bool content(float2 target, out float2 p) {
    float2 ratio = float2(targetSize) / float2(sourceSize);
    float scale = min(ratio.x, ratio.y);
    float2 size = float2(sourceSize) * scale;
    if (all(abs(size - float2(targetSize)) < 1)) {
        p = (target + 0.5) / ratio - 0.5;
        return true;
    }
    float2 at = target + 0.5 - (float2(targetSize) - size) * 0.5;
    p = at / scale - 0.5;
    return all(at >= 0) && all(at < size);
}
float3 nonlinear(float2 target) {
    float3 rgb;
    if (all(sourceSize == targetSize)) {
        rgb = load(int2(target));
    } else {
        float2 p;
        if (!content(target, p)) return float3(0, 0, 0);
        int2 at = int2(floor(p));
        float2 f = frac(p);
        rgb = lerp(lerp(load(at), load(at + int2(1, 0)), f.x),
                   lerp(load(at + int2(0, 1)), load(at + int2(1, 1)), f.x), f.y);
    }
    if (hdr == 0 || pixel == 2) return rgb;
    return pq(gamut2020(rgb) / 125.0);
}
float3 weights() { return colorMatrix == 2 ? float3(0.2627, 0.6780, 0.0593) : colorMatrix == 0 ? float3(0.299, 0.587, 0.114) : float3(0.2126, 0.7152, 0.0722); }
// The luma code of a pixel, and the chroma code of a 2x2 block's average.
// Shared by the graphics and compute conversions, so both write the same codes.
float lumaCode(float3 rgb) {
    float y = dot(rgb, weights());
    return tenBit != 0 ? floor(clamp((fullRange != 0 ? 1023 * y : 64 + 876 * y), 0, 1023) + 0.5) * 64 / 65535.0
                       : floor(clamp((fullRange != 0 ? 255 * y : 16 + 219 * y), 0, 255) + 0.5) / 255.0;
}
float2 chromaCode(float3 c00, float3 c10, float3 c01, float3 c11) {
    float3 rgb = (c00 + c10 + c01 + c11) * 0.25;
    float3 k = weights();
    float y = dot(rgb, k);
    float2 uv = float2((rgb.b - y) / (2 * (1 - k.b)), (rgb.r - y) / (2 * (1 - k.r)));
    return tenBit != 0 ? floor(clamp(512 + (fullRange != 0 ? 1023 : 896) * uv, 0, 1023) + 0.5) * 64 / 65535.0
                       : floor(clamp(128 + (fullRange != 0 ? 255 : 224) * uv, 0, 255) + 0.5) / 255.0;
}
float4 luma(float4 p : SV_Position) : SV_Target {
    return float4(lumaCode(nonlinear(p.xy - 0.5)), 0, 0, 1);
}
float4 chroma(float4 p : SV_Position) : SV_Target {
    float2 at = floor(p.xy) * 2;
    return float4(chromaCode(nonlinear(at), nonlinear(at + float2(1, 0)),
                             nonlinear(at + float2(0, 1)), nonlinear(at + float2(1, 1))), 0, 1);
}
// The same conversion on a compute queue, which keeps running while a game
// fills the graphics queue. The two views address the luma and chroma planes.
RWTexture2D<float> lumaPlane : register(u0);
RWTexture2D<float2> chromaPlane : register(u1);
// One thread per 2x2 block: its four pixels are converted once for their
// four luma codes and the block's chroma, instead of in a luma and a chroma
// pass that each converted every pixel.
[numthreads(8, 8, 1)]
void yuv420_cs(uint3 id : SV_DispatchThreadID) {
    if (any(id.xy >= targetSize / 2)) return;
    uint2 at = id.xy * 2;
    float3 c00 = nonlinear(float2(at));
    float3 c10 = nonlinear(float2(at + uint2(1, 0)));
    float3 c01 = nonlinear(float2(at + uint2(0, 1)));
    float3 c11 = nonlinear(float2(at + uint2(1, 1)));
    lumaPlane[at] = lumaCode(c00);
    lumaPlane[at + uint2(1, 0)] = lumaCode(c10);
    lumaPlane[at + uint2(0, 1)] = lumaCode(c01);
    lumaPlane[at + uint2(1, 1)] = lumaCode(c11);
    chromaPlane[id.xy] = chromaCode(c00, c10, c01, c11);
}
float3 yuv444(float2 p) {
    float3 rgb = nonlinear(p);
    float3 k = weights();
    float y = dot(rgb, k);
    return float3(y, (rgb.b - y) / (2 * (1 - k.b)), (rgb.r - y) / (2 * (1 - k.r)));
}
// DXGI AYUV has V, U, Y, A byte order in the compatible RGBA render view.
float4 packed444(float4 p : SV_Position) : SV_Target {
    float3 yuv = yuv444(p.xy - 0.5);
    float y = floor(clamp(fullRange != 0 ? 255 * yuv.x : 16 + 219 * yuv.x, 0, 255) + 0.5);
    float2 uv = floor(clamp(128 + (fullRange != 0 ? 255 : 224) * yuv.yz, 0, 255) + 0.5);
    return float4(uv.y, uv.x, y, 255) / 255;
}
// CUDA imports this single R16_UINT texture into pitched Y/U/V device memory.
// NVENC's 16-bit 4:4:4 container stores 10-bit codes in the upper bits.
uint planar444(float4 p : SV_Position) : SV_Target {
    uint plane = uint(p.y) / targetSize.y;
    float3 yuv = yuv444(float2(p.x - 0.5, p.y - 0.5 - plane * targetSize.y));
    float code = plane == 0 ? (fullRange != 0 ? 1023 * yuv.x : 64 + 876 * yuv.x)
                           : 512 + (fullRange != 0 ? 1023 : 896) * yuv[plane];
    return uint(floor(clamp(code, 0, 1023) + 0.5)) << 6;
}
// PyroWave's planes hold codes over their full range (code / 1023 or 255),
// one plane each. Shared by the graphics and compute conversions.
float pyroLuma(float3 rgb) {
    float y = dot(rgb, weights());
    float maximum = tenBit != 0 ? 1023 : 255;
    float code = tenBit != 0 ? (fullRange != 0 ? 1023*y : 64+876*y) : (fullRange != 0 ? 255*y : 16+219*y);
    return floor(clamp(code,0,maximum)+0.5)/maximum;
}
float2 pyroChroma(float3 rgb) {
    float3 k = weights(); float y = dot(rgb,k);
    float2 uv = float2((rgb.b-y)/(2*(1-k.b)),(rgb.r-y)/(2*(1-k.r)));
    float maximum = tenBit != 0 ? 1023 : 255;
    float2 code = tenBit != 0 ? 512+(fullRange != 0 ? 1023 : 896)*uv : 128+(fullRange != 0 ? 255 : 224)*uv;
    return floor(clamp(code,0,maximum)+0.5)/maximum;
}
// 4:4:4 (padding != 0) takes each pixel's colour; 4:2:0 a 2x2 block's average.
float2 pyroBlock(float3 c00, float3 c10, float3 c01, float3 c11) {
    return pyroChroma((c00 + c10 + c01 + c11) * 0.25);
}
float4 pyro_y(float4 p : SV_Position) : SV_Target {
    return float4(pyroLuma(nonlinear(p.xy - 0.5)),0,0,1);
}
float2 pyro_chroma(float2 p) {
    if (padding != 0) return pyroChroma(nonlinear(p - 0.5));
    float2 at = floor(p)*2;
    return pyroBlock(nonlinear(at), nonlinear(at+float2(1,0)), nonlinear(at+float2(0,1)), nonlinear(at+float2(1,1)));
}
float4 pyro_u(float4 p : SV_Position) : SV_Target { return float4(pyro_chroma(p.xy).x,0,0,1); }
float4 pyro_v(float4 p : SV_Position) : SV_Target { return float4(pyro_chroma(p.xy).y,0,0,1); }
// PyroWave's three planes on the compute queue, in one pass over 2x2 blocks,
// beside a game that fills the graphics queue.
RWTexture2D<float> pyroY : register(u2);
RWTexture2D<float> pyroU : register(u3);
RWTexture2D<float> pyroV : register(u4);
[numthreads(8, 8, 1)]
void pyro_cs(uint3 id : SV_DispatchThreadID) {
    if (any(id.xy >= targetSize / 2)) return;
    uint2 at = id.xy * 2;
    float3 c00 = nonlinear(float2(at));
    float3 c10 = nonlinear(float2(at + uint2(1, 0)));
    float3 c01 = nonlinear(float2(at + uint2(0, 1)));
    float3 c11 = nonlinear(float2(at + uint2(1, 1)));
    pyroY[at] = pyroLuma(c00);
    pyroY[at + uint2(1, 0)] = pyroLuma(c10);
    pyroY[at + uint2(0, 1)] = pyroLuma(c01);
    pyroY[at + uint2(1, 1)] = pyroLuma(c11);
    if (padding != 0) {
        float2 c = pyroChroma(c00); pyroU[at] = c.x; pyroV[at] = c.y;
        c = pyroChroma(c10); pyroU[at + uint2(1, 0)] = c.x; pyroV[at + uint2(1, 0)] = c.y;
        c = pyroChroma(c01); pyroU[at + uint2(0, 1)] = c.x; pyroV[at + uint2(0, 1)] = c.y;
        c = pyroChroma(c11); pyroU[at + uint2(1, 1)] = c.x; pyroV[at + uint2(1, 1)] = c.y;
    } else {
        float2 c = pyroBlock(c00, c10, c01, c11);
        pyroU[id.xy] = c.x;
        pyroV[id.xy] = c.y;
    }
}
