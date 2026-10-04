// Independent C consumer of the Rust DLL. Never linked into the streaming host.
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>
#include <dxgi.h>
#include <stdio.h>
typedef unsigned (__cdecl *abi_fn)(void);
typedef void *(__cdecl *create_fn)(void *, unsigned *);
typedef void (__cdecl *destroy_fn)(void *);
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    HMODULE dll = LoadLibraryA(argv[1]); if (!dll) return 3;
    abi_fn abi = (abi_fn)GetProcAddress(dll,"butterpollo_truehdr_abi");
    create_fn create = (create_fn)GetProcAddress(dll,"butterpollo_truehdr_create");
    destroy_fn destroy = (destroy_fn)GetProcAddress(dll,"butterpollo_truehdr_destroy");
    if (!abi || !create || !destroy || abi() != 1) return 4;
    ID3D11Device *device=NULL; ID3D11DeviceContext *context=NULL;
    if (FAILED(D3D11CreateDevice(NULL,D3D_DRIVER_TYPE_HARDWARE,NULL,D3D11_CREATE_DEVICE_BGRA_SUPPORT,NULL,0,D3D11_SDK_VERSION,&device,NULL,&context))) return 5;
    IDXGIDevice *dxgi=NULL; IDXGIAdapter *adapter=NULL; DXGI_ADAPTER_DESC desc;
    const IID dxgi_iid = {0x54ec77fa,0x1377,0x44e6,{0x8c,0x32,0x88,0xfd,0x5f,0x44,0xc8,0x4c}};
    if (FAILED(ID3D11Device_QueryInterface(device,&dxgi_iid,(void **)&dxgi))) return 6;
    IDXGIDevice_GetAdapter(dxgi,&adapter); IDXGIAdapter_GetDesc(adapter,&desc);
    unsigned error=0; void *state=create(device,&error);
    if (desc.VendorId != 0x10de && (state || !error)) return 7;
    if (state) destroy(state);
    printf("TRUEHDR ABI PASS vendor=%04x initialization=%s code=%08x\n",desc.VendorId,state?"ready":"unsupported",error);
    IDXGIAdapter_Release(adapter); IDXGIDevice_Release(dxgi); ID3D11DeviceContext_Release(context); ID3D11Device_Release(device); FreeLibrary(dll); return 0;
}
