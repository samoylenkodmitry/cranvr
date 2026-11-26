// cranvr's test program for Wine patch 0009: a VkImage on a Metal texture of an IOSurface.
//
// It makes a Metal texture on an IOSurface with cranvr's DXMT fork (winemetal.dll), writes a
// pattern into the texture with Metal, makes a VkImage on the texture with
// VkImportMetalTextureInfoEXT and copies the image into a buffer with Vulkan. The buffer holds the
// pattern only when winevulkan passed the import to MoltenVK, so that the image lies on the
// texture's memory. Without the patch the image is an empty image of its own.
//
// It prints "metal_texture_check: PASS" or "metal_texture_check: FAIL" with the first wrong pixel.

#include <vulkan/vulkan.h>
#include <vulkan/vulkan_metal.h>
#include <windows.h>

#include <cstdint>
#include <cstdio>

namespace {

typedef uint64_t ObjHandle;

// struct WMTTextureInfo and the structures of MTLTexture_replaceRegion of DXMT v0.80 (winemetal.h)
struct WmtTextureInfo {
    uint32_t pixelFormat, width, height, depth, arrayLength;
    uint32_t type : 8, mipmapLevelCount : 8, sampleCount : 8, usage : 8;
    uint64_t options;
    uint32_t reserved, machPort;
    uint64_t gpuResourceId;
};
static_assert(sizeof(WmtTextureInfo) == 48, "WMTTextureInfo of DXMT v0.80");
struct WmtOrigin { uint64_t x, y, z; };
struct WmtSize { uint64_t width, height, depth; };
struct WmtMemoryPointer { void* ptr; };

template <typename T>
T vkStruct(VkStructureType type) {
    T value{};
    value.sType = type;
    return value;
}

int fail(const char* what, long long value) {
    std::printf("metal_texture_check: FAIL %s (%lld)\n", what, value);
    return 1;
}

}  // namespace

int main() {
    constexpr uint32_t kSide = 64;
    HMODULE vulkan = LoadLibraryA("vulkan-1.dll");
    HMODULE metal = LoadLibraryA("winemetal.dll");
    if (vulkan == nullptr || metal == nullptr) {
        return fail("LoadLibrary of vulkan-1.dll or winemetal.dll", 0);
    }
    auto copyAllDevices = reinterpret_cast<ObjHandle (*)()>(reinterpret_cast<void*>(GetProcAddress(metal, "WMTCopyAllDevices")));
    auto arrayObject = reinterpret_cast<ObjHandle (*)(ObjHandle, uint64_t)>(reinterpret_cast<void*>(GetProcAddress(metal, "NSArray_object")));
    auto newTexture = reinterpret_cast<ObjHandle (*)(ObjHandle, WmtTextureInfo*)>(
        reinterpret_cast<void*>(GetProcAddress(metal, "MTLDevice_newTextureFromIOSurface")));
    auto replaceRegion = reinterpret_cast<void (*)(ObjHandle, WmtOrigin, WmtSize, uint64_t, uint64_t, WmtMemoryPointer, uint64_t, uint64_t)>(
        reinterpret_cast<void*>(GetProcAddress(metal, "MTLTexture_replaceRegion")));
    if (copyAllDevices == nullptr || arrayObject == nullptr || newTexture == nullptr || replaceRegion == nullptr) {
        return fail("winemetal.dll is not cranvr's DXMT fork", 0);
    }
    WmtTextureInfo info{};
    info.pixelFormat = 80;  // BGRA8Unorm
    info.width = kSide;
    info.height = kSide;
    info.depth = 1;
    info.arrayLength = 1;
    info.type = 2;  // 2D
    info.mipmapLevelCount = 1;
    info.sampleCount = 1;
    info.usage = 1 | 4 | 16;  // shader read, render target, pixel format view
    const ObjHandle texture = newTexture(arrayObject(copyAllDevices(), 0), &info);
    if (texture == 0 || info.reserved == 0) {
        return fail("MTLDevice_newTextureFromIOSurface", 0);
    }
    static uint32_t pattern[kSide * kSide];
    for (uint32_t y = 0; y < kSide; ++y) {
        for (uint32_t x = 0; x < kSide; ++x) {
            pattern[y * kSide + x] = 0xff000000u | (0x5au << 16) | (y << 8) | x;
        }
    }
    replaceRegion(texture, {0, 0, 0}, {kSide, kSide, 1}, 0, 0, {pattern}, kSide * 4, 0);

    auto getInstanceProcAddr = reinterpret_cast<PFN_vkGetInstanceProcAddr>(
        reinterpret_cast<void*>(GetProcAddress(vulkan, "vkGetInstanceProcAddr")));
    auto createInstance = reinterpret_cast<PFN_vkCreateInstance>(getInstanceProcAddr(VK_NULL_HANDLE, "vkCreateInstance"));
    auto appInfo = vkStruct<VkApplicationInfo>(VK_STRUCTURE_TYPE_APPLICATION_INFO);
    appInfo.apiVersion = VK_API_VERSION_1_1;
    auto instanceInfo = vkStruct<VkInstanceCreateInfo>(VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO);
    instanceInfo.pApplicationInfo = &appInfo;
    VkInstance instance = VK_NULL_HANDLE;
    VkResult result = createInstance(&instanceInfo, nullptr, &instance);
    if (result != VK_SUCCESS) {
        return fail("vkCreateInstance", result);
    }
#define INSTANCE(name) auto name = reinterpret_cast<PFN_##name>(getInstanceProcAddr(instance, #name))
    INSTANCE(vkEnumeratePhysicalDevices);
    INSTANCE(vkGetPhysicalDeviceMemoryProperties);
    INSTANCE(vkCreateDevice);
    INSTANCE(vkGetDeviceProcAddr);
#undef INSTANCE
    uint32_t count = 1;
    VkPhysicalDevice physical = VK_NULL_HANDLE;
    vkEnumeratePhysicalDevices(instance, &count, &physical);
    float priority = 1.0f;
    auto queueInfo = vkStruct<VkDeviceQueueCreateInfo>(VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO);
    queueInfo.queueCount = 1;
    queueInfo.pQueuePriorities = &priority;
    auto deviceInfo = vkStruct<VkDeviceCreateInfo>(VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO);
    deviceInfo.queueCreateInfoCount = 1;
    deviceInfo.pQueueCreateInfos = &queueInfo;
    VkDevice device = VK_NULL_HANDLE;
    if (physical == VK_NULL_HANDLE || (result = vkCreateDevice(physical, &deviceInfo, nullptr, &device)) != VK_SUCCESS) {
        return fail("vkCreateDevice", result);
    }
#define DEVICE(name) auto name = reinterpret_cast<PFN_##name>(vkGetDeviceProcAddr(device, #name))
    DEVICE(vkGetDeviceQueue);
    DEVICE(vkCreateImage);
    DEVICE(vkGetImageMemoryRequirements);
    DEVICE(vkAllocateMemory);
    DEVICE(vkBindImageMemory);
    DEVICE(vkCreateBuffer);
    DEVICE(vkGetBufferMemoryRequirements);
    DEVICE(vkBindBufferMemory);
    DEVICE(vkMapMemory);
    DEVICE(vkCreateCommandPool);
    DEVICE(vkAllocateCommandBuffers);
    DEVICE(vkBeginCommandBuffer);
    DEVICE(vkEndCommandBuffer);
    DEVICE(vkCmdPipelineBarrier);
    DEVICE(vkCmdCopyImageToBuffer);
    DEVICE(vkQueueSubmit);
    DEVICE(vkQueueWaitIdle);
#undef DEVICE
    VkQueue queue = VK_NULL_HANDLE;
    vkGetDeviceQueue(device, 0, 0, &queue);
    VkPhysicalDeviceMemoryProperties memory{};
    vkGetPhysicalDeviceMemoryProperties(physical, &memory);
    auto memoryType = [&](uint32_t bits, VkMemoryPropertyFlags wanted) {
        for (uint32_t index = 0; index < memory.memoryTypeCount; ++index) {
            if ((bits & (1u << index)) != 0 && (memory.memoryTypes[index].propertyFlags & wanted) == wanted) {
                return index;
            }
        }
        return 0u;
    };

    auto import = vkStruct<VkImportMetalTextureInfoEXT>(VK_STRUCTURE_TYPE_IMPORT_METAL_TEXTURE_INFO_EXT);
    import.plane = VK_IMAGE_ASPECT_PLANE_0_BIT;
    import.mtlTexture = reinterpret_cast<MTLTexture_id>(static_cast<uintptr_t>(texture));
    auto imageInfo = vkStruct<VkImageCreateInfo>(VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO);
    imageInfo.pNext = &import;
    imageInfo.imageType = VK_IMAGE_TYPE_2D;
    imageInfo.format = VK_FORMAT_B8G8R8A8_UNORM;
    imageInfo.extent = {kSide, kSide, 1};
    imageInfo.mipLevels = 1;
    imageInfo.arrayLayers = 1;
    imageInfo.samples = VK_SAMPLE_COUNT_1_BIT;
    imageInfo.tiling = VK_IMAGE_TILING_OPTIMAL;
    imageInfo.usage = VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT;
    VkImage image = VK_NULL_HANDLE;
    if ((result = vkCreateImage(device, &imageInfo, nullptr, &image)) != VK_SUCCESS) {
        return fail("vkCreateImage", result);
    }
    VkMemoryRequirements imageRequirements{};
    vkGetImageMemoryRequirements(device, image, &imageRequirements);
    auto imageAllocate = vkStruct<VkMemoryAllocateInfo>(VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO);
    imageAllocate.allocationSize = imageRequirements.size;
    imageAllocate.memoryTypeIndex = memoryType(imageRequirements.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
    VkDeviceMemory imageMemory = VK_NULL_HANDLE;
    vkAllocateMemory(device, &imageAllocate, nullptr, &imageMemory);
    vkBindImageMemory(device, image, imageMemory, 0);

    auto bufferInfo = vkStruct<VkBufferCreateInfo>(VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO);
    bufferInfo.size = sizeof(pattern);
    bufferInfo.usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT;
    VkBuffer buffer = VK_NULL_HANDLE;
    vkCreateBuffer(device, &bufferInfo, nullptr, &buffer);
    VkMemoryRequirements bufferRequirements{};
    vkGetBufferMemoryRequirements(device, buffer, &bufferRequirements);
    auto bufferAllocate = vkStruct<VkMemoryAllocateInfo>(VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO);
    bufferAllocate.allocationSize = bufferRequirements.size;
    bufferAllocate.memoryTypeIndex = memoryType(bufferRequirements.memoryTypeBits,
                                                VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
    VkDeviceMemory bufferMemory = VK_NULL_HANDLE;
    vkAllocateMemory(device, &bufferAllocate, nullptr, &bufferMemory);
    vkBindBufferMemory(device, buffer, bufferMemory, 0);

    auto poolInfo = vkStruct<VkCommandPoolCreateInfo>(VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO);
    VkCommandPool pool = VK_NULL_HANDLE;
    vkCreateCommandPool(device, &poolInfo, nullptr, &pool);
    auto allocateInfo = vkStruct<VkCommandBufferAllocateInfo>(VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO);
    allocateInfo.commandPool = pool;
    allocateInfo.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
    allocateInfo.commandBufferCount = 1;
    VkCommandBuffer commands = VK_NULL_HANDLE;
    vkAllocateCommandBuffers(device, &allocateInfo, &commands);
    auto beginInfo = vkStruct<VkCommandBufferBeginInfo>(VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO);
    vkBeginCommandBuffer(commands, &beginInfo);
    // The image keeps what Metal wrote: an old layout of GENERAL, not UNDEFINED
    auto barrier = vkStruct<VkImageMemoryBarrier>(VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER);
    barrier.dstAccessMask = VK_ACCESS_TRANSFER_READ_BIT;
    barrier.oldLayout = VK_IMAGE_LAYOUT_GENERAL;
    barrier.newLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
    barrier.srcQueueFamilyIndex = barrier.dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
    barrier.image = image;
    barrier.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    vkCmdPipelineBarrier(commands, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, nullptr, 0,
                         nullptr, 1, &barrier);
    VkBufferImageCopy copy{};
    copy.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
    copy.imageExtent = {kSide, kSide, 1};
    vkCmdCopyImageToBuffer(commands, image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, buffer, 1, &copy);
    vkEndCommandBuffer(commands);
    auto submit = vkStruct<VkSubmitInfo>(VK_STRUCTURE_TYPE_SUBMIT_INFO);
    submit.commandBufferCount = 1;
    submit.pCommandBuffers = &commands;
    vkQueueSubmit(queue, 1, &submit, VK_NULL_HANDLE);
    vkQueueWaitIdle(queue);

    const uint32_t* read = nullptr;
    vkMapMemory(device, bufferMemory, 0, sizeof(pattern), 0, reinterpret_cast<void**>(const_cast<uint32_t**>(&read)));
    for (uint32_t index = 0; index < kSide * kSide; ++index) {
        if (read[index] != pattern[index]) {
            std::printf("metal_texture_check: FAIL pixel %u of IOSurface %u is %#010x, Metal wrote %#010x: the image does not "
                        "lie on the Metal texture\n", index, info.reserved, read[index], pattern[index]);
            return 1;
        }
    }
    std::printf("metal_texture_check: PASS: Vulkan read the %ux%u pixels that Metal wrote into IOSurface %u\n", kSide,
                kSide, info.reserved);
    return 0;
}
