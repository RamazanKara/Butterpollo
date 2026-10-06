# Moonlight 6.2.0 CSV shutdown compatibility patch

The [client source patch](moonlight-v6.2.0-cli-cached-artwork.patch) targets Moonlight Qt `v6.2.0` (`de2467e433821664cdd2224aad8c89a625be1ad9`). It is optional source for a custom client build. The official Moonlight 6.2.0 binary is unchanged and still has the cold-cache CSV limitation; this is not a Butterpollo runtime fix.

`list --csv` can print its rows and then hang on shutdown with missing artwork. The [CSV implementation](https://github.com/moonlight-stream/moonlight-qt/blob/v6.2.0/app/cli/listapps.cpp#L140) starts [asynchronous artwork workers](https://github.com/moonlight-stream/moonlight-qt/blob/v6.2.0/app/backend/boxartmanager.cpp#L72) immediately before exiting. In the isolated official-client reproduction, shutdown waited in the artwork thread pool while workers waited in Qt event loops; no artwork request reached the host.

The patch makes CSV read only the artwork cache. Cached URLs and missing-image placeholders are preserved, but CSV no longer opportunistically downloads missing artwork. Existing GUI callers retain asynchronous downloads, retries, cache writes and completion signals. Plain `list`, or CSV after genuine artwork has been cached, remain working options with the official client.

Apply to an unmodified Moonlight source checkout:

```sh
git checkout v6.2.0
git apply --check /path/to/moonlight-v6.2.0-cli-cached-artwork.patch
git apply /path/to/moonlight-v6.2.0-cli-cached-artwork.patch
```

Then rebuild Moonlight using that checkout's build instructions. The patch does not modify an existing executable. Its SHA-256 is `35650fe7068713303be0f126a515a835b31004bf35c47ab01481b3b2de5e46a8`.

Validation: eight patched cases passed using the actual original/patched `BoxArtManager` translation unit, real Qt 6.4.2 on Linux and GCC 13.3.0 with warnings as errors. Only HTTP/computer and cache-directory boundaries were stubbed. Cases cover cold, empty and warm caches, multiple apps, GUI retries, successful asynchronous completion and PNG caching. Three negative controls expose the original CLI's unwanted background requests; five preserved-behavior baseline cases pass. Patch application and the CLI's explicit cache-only call were also checked.

This is a focused causal regression, not a full Windows Moonlight build or a patched official-client end-to-end test. The HTTP stub returns promptly: it proves removal of the unwanted worker creation, rather than reproducing Qt's network shutdown hang. A custom Windows client still needs validation before distribution. No upstream submission or official executable replacement is included.
