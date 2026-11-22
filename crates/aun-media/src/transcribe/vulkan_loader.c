/*
 * Stand-in for the Vulkan loader library, linked into aun_core with the `whisper-vulkan` feature
 * (see build.rs). ggml-vulkan resolves almost every Vulkan call through vkGetInstanceProcAddr; it
 * links only the three functions below directly. They forward to the real loader, opened at run
 * time. Without one, the global commands answer VK_ERROR_INITIALIZATION_FAILED, ggml's Vulkan
 * start-up throws and is caught (ggml_backend_vk_reg returns no backend), and whisper uses the CPU.
 *
 * AUN_VULKAN_LOADER: unset opens the system loader; "none" means no loader (the CPU only); any other
 * value is the loader's path.
 */
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#if defined(_WIN32)
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#define AUN_VKAPI __stdcall
#else
#include <dlfcn.h>
#define AUN_VKAPI
#endif

typedef void (AUN_VKAPI *aun_vk_void_fn)(void);
typedef aun_vk_void_fn (AUN_VKAPI *aun_vk_gipa_fn)(void *instance, const char *name);
typedef void (AUN_VKAPI *aun_vk_features2_fn)(void *physical_device, void *features);
typedef void (AUN_VKAPI *aun_vk_copy_buffer_fn)(void *command_buffer, void *src, void *dst, uint32_t count, const void *regions);

#define AUN_VK_ERROR_INITIALIZATION_FAILED (-3)

/* Opened once; a race only opens the library twice, which the platform reference-counts. */
static volatile int aun_vk_tried;
static void *volatile aun_vk_library;

static void *aun_vk_open(void) {
    if (aun_vk_tried) {
        return aun_vk_library;
    }
    const char *path = getenv("AUN_VULKAN_LOADER");
    void *library = NULL;
    if (path == NULL || strcmp(path, "none") != 0) {
#if defined(_WIN32)
        library = (void *)LoadLibraryA(path != NULL && path[0] != '\0' ? path : "vulkan-1.dll");
#else
        if (path != NULL && path[0] != '\0') {
            library = dlopen(path, RTLD_NOW | RTLD_LOCAL);
        } else {
#if defined(__APPLE__)
            library = dlopen("libvulkan.1.dylib", RTLD_NOW | RTLD_LOCAL);
#else
            library = dlopen("libvulkan.so.1", RTLD_NOW | RTLD_LOCAL);
#endif
        }
#endif
    }
    aun_vk_library = library;
    aun_vk_tried = 1;
    return library;
}

static void *aun_vk_symbol(const char *name) {
    void *library = aun_vk_open();
    if (library == NULL) {
        return NULL;
    }
#if defined(_WIN32)
    return (void *)GetProcAddress((HMODULE)library, name);
#else
    return dlsym(library, name);
#endif
}

static int32_t AUN_VKAPI aun_vk_no_version(uint32_t *version) {
    if (version != NULL) {
        *version = 0;
    }
    return AUN_VK_ERROR_INITIALIZATION_FAILED;
}

static int32_t AUN_VKAPI aun_vk_no_properties(const char *layer, uint32_t *count, void *properties) {
    (void)layer;
    (void)properties;
    if (count != NULL) {
        *count = 0;
    }
    return AUN_VK_ERROR_INITIALIZATION_FAILED;
}

static int32_t AUN_VKAPI aun_vk_no_layers(uint32_t *count, void *properties) {
    (void)properties;
    if (count != NULL) {
        *count = 0;
    }
    return AUN_VK_ERROR_INITIALIZATION_FAILED;
}

static int32_t AUN_VKAPI aun_vk_no_instance(const void *info, const void *allocator, void **instance) {
    (void)info;
    (void)allocator;
    if (instance != NULL) {
        *instance = NULL;
    }
    return AUN_VK_ERROR_INITIALIZATION_FAILED;
}

/* Each forwarded function is looked up once: vkCmdCopyBuffer runs for every upload to the GPU */
static void *volatile aun_vk_gipa;
static void *volatile aun_vk_features2;
static void *volatile aun_vk_copy_buffer;

static void *aun_vk_cached(void *volatile *slot, const char *name) {
    void *found = *slot;
    if (found == NULL) {
        found = aun_vk_symbol(name);
        *slot = found;
    }
    return found;
}

aun_vk_void_fn AUN_VKAPI vkGetInstanceProcAddr(void *instance, const char *name) {
    aun_vk_gipa_fn real = (aun_vk_gipa_fn)aun_vk_cached(&aun_vk_gipa, "vkGetInstanceProcAddr");
    if (real != NULL) {
        return real(instance, name);
    }
    if (instance != NULL || name == NULL) {
        return NULL;
    }
    if (strcmp(name, "vkEnumerateInstanceVersion") == 0) {
        return (aun_vk_void_fn)aun_vk_no_version;
    }
    if (strcmp(name, "vkEnumerateInstanceExtensionProperties") == 0) {
        return (aun_vk_void_fn)aun_vk_no_properties;
    }
    if (strcmp(name, "vkEnumerateInstanceLayerProperties") == 0) {
        return (aun_vk_void_fn)aun_vk_no_layers;
    }
    if (strcmp(name, "vkCreateInstance") == 0) {
        return (aun_vk_void_fn)aun_vk_no_instance;
    }
    return NULL;
}

/* Only reached with a device, so the loader is open; both are core exports of every loader. */
void AUN_VKAPI vkGetPhysicalDeviceFeatures2(void *physical_device, void *features) {
    aun_vk_features2_fn real = (aun_vk_features2_fn)aun_vk_cached(&aun_vk_features2, "vkGetPhysicalDeviceFeatures2");
    if (real != NULL) {
        real(physical_device, features);
    }
}

void AUN_VKAPI vkCmdCopyBuffer(void *command_buffer, void *src, void *dst, uint32_t count, const void *regions) {
    aun_vk_copy_buffer_fn real = (aun_vk_copy_buffer_fn)aun_vk_cached(&aun_vk_copy_buffer, "vkCmdCopyBuffer");
    if (real != NULL) {
        real(command_buffer, src, dst, count, regions);
    }
}
