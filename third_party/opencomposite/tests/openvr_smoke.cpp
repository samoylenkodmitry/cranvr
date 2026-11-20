// A small OpenVR game for cranvr's tests. It does what an OpenVR game does at the start and in each
// frame, through OpenComposite and the OpenXR runtime that cranvr-host serves:
//
//   - It finds the OpenVR runtime as Valve's openvr_api.dll does, through
//     %LOCALAPPDATA%\openvr\openvrpaths.vrpath, loads bin\vrclient_x64.dll from there and starts it as
//     VRApplication_Scene. Without that file it fails, so the test fails when cranvr's choice of
//     OpenComposite does not reach the prefix.
//   - It makes a Direct3D 11 device, a render target for each eye at the recommended size, and runs
//     WaitGetPoses, Submit and PostPresentHandoff for the time that --seconds says.
//   - It reads the head pose and, with --expect-input, the legacy controller state, which the
//     scripted controllers of cranvr-probe move (a pull of the left grip, the right trigger at 0.75,
//     the A button).
//
// The last line is "openvr_smoke: PASS" or "openvr_smoke: FAIL ...". The program takes no part of
// Valve's code except the headers of OpenComposite's OpenVRHeaders (BSD 3-Clause).
//
// With --load-only --runtime DIR it only loads DIR\bin\vrclient_x64.dll and asks it for its core, as
// scripts/setup-wine.sh does to check an install. The runtime is then not found by the path file.
//
// --format rgba|bgra|bgra-typeless picks the format of the eye textures: R8G8B8A8_UNORM (the default; the runtime
// of cranvr offers B8G8R8A8 only, so OpenComposite converts it with a draw), B8G8R8A8_UNORM (copied as it is) and
// B8G8R8A8_TYPELESS with a UNORM view, which is the texture that a game such as Serious Sam VR submits.
//
// --actions touch|vive|both tests IVRInput, the way a game that ships an action manifest uses it. The program
// writes a manifest and the default bindings that such a game ships (for Touch controllers, for the Vive wand or
// for both) to a folder in %TEMP%, sets the manifest, and in each frame it updates the action set and reads the
// actions that the probe's scripted controllers drive (the left grip, the right trigger, A, the right stick) and
// both hands' poses. It also pulses the left hand's haptic output while the grip is down. With --expect-input the
// run fails when an action does not fire.
//
// The frame loop reports where the time goes and how long each frame lasted (the intervals between the
// returns of WaitGetPoses, as a histogram in periods of the display's refresh). --no-input leaves the
// controller reads out to show what they cost.
//
// Usage: openvr_smoke.exe [--seconds N] [--expect-input] [--no-input] [--actions MODE] [--format F]
//                         [--load-only --runtime DIR]

#ifndef NOMINMAX
#define NOMINMAX
#endif
#define WIN32_LEAN_AND_MEAN
#include <windows.h>

#include <d3d11.h>
#include <dxgi.h>

#include <algorithm>
#include <chrono>
#include <cstdarg>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <direct.h>
#include <fstream>
#include <sstream>
#include <string>
#include <utility>
#include <vector>

// The header declares the API as plain functions when it is built static: this program defines them
#define OPENVR_BUILD_STATIC
#include "openvr-2.5.1.h"
#include "custom_interfaces/IVRClientCore_003.h"

namespace {

void say(const char* format, ...) __attribute__((format(printf, 1, 2)));
void say(const char* format, ...)
{
	va_list args;
	va_start(args, format);
	std::printf("openvr_smoke: ");
	std::vprintf(format, args);
	std::printf("\n");
	std::fflush(stdout);
	va_end(args);
}

int fail(const char* what)
{
	say("FAIL %s", what);
	return 1;
}

// === The part of openvr_api.dll that loads the runtime ===
//
// Valve's loader reads openvrpaths.vrpath, a JSON file with the runtime's folder, and loads
// <runtime>\bin\vrclient_x64.dll. It asks that DLL for the IVRClientCore and starts it.

HMODULE g_client_dll = nullptr;
vr::IVRClientCore_003::IVRClientCore* g_core = nullptr;
uint32_t g_token = 0;
std::string g_runtime_path;
bool g_runtime_given = false; // --runtime: no path file

std::string vrpath_file()
{
	const char* local = std::getenv("LOCALAPPDATA");
	return std::string(local ? local : "") + "\\openvr\\openvrpaths.vrpath";
}

// The first string of the "runtime" array, with the JSON escapes of a path undone
bool read_runtime_path(const std::string& file, std::string& path)
{
	std::ifstream in(file, std::ios::binary);
	if (!in) {
		return false;
	}
	std::stringstream text;
	text << in.rdbuf();
	const std::string json = text.str();
	size_t at = json.find("\"runtime\"");
	if (at == std::string::npos || (at = json.find('[', at)) == std::string::npos || (at = json.find('"', at)) == std::string::npos) {
		return false;
	}
	path.clear();
	for (size_t i = at + 1; i < json.size() && json[i] != '"'; i++) {
		if (json[i] == '\\' && i + 1 < json.size()) {
			i++;
		}
		path += json[i];
	}
	return !path.empty();
}

vr::EVRInitError load_runtime()
{
	if (g_core) {
		return vr::VRInitError_None;
	}
	const std::string file = vrpath_file();
	if (!g_runtime_given && !read_runtime_path(file, g_runtime_path)) {
		say("no runtime in %s", file.c_str());
		return vr::VRInitError_Init_PathRegistryNotFound;
	}
	say("runtime path %s (%s)", g_runtime_path.c_str(), g_runtime_given ? "given" : file.c_str());
	const std::string dll = g_runtime_path + "\\bin\\vrclient_x64.dll";
	g_client_dll = LoadLibraryExA(dll.c_str(), nullptr, LOAD_WITH_ALTERED_SEARCH_PATH);
	if (!g_client_dll) {
		say("cannot load %s (error %lu)", dll.c_str(), GetLastError());
		return vr::VRInitError_Init_VRClientDLLNotFound;
	}
	using Factory = void* (*)(const char*, int*);
	const auto factory = reinterpret_cast<Factory>(reinterpret_cast<void*>(GetProcAddress(g_client_dll, "VRClientCoreFactory")));
	if (!factory) {
		return vr::VRInitError_Init_FactoryNotFound;
	}
	int code = 0;
	g_core = static_cast<vr::IVRClientCore_003::IVRClientCore*>(factory(vr::IVRClientCore_003::IVRClientCore_Version, &code));
	return g_core ? vr::VRInitError_None : vr::VRInitError_Init_InterfaceNotFound;
}

} // namespace

VR_INTERFACE uint32_t VR_CALLTYPE VR_InitInternal2(vr::EVRInitError* error, vr::EVRApplicationType type, const char* startup)
{
	vr::EVRInitError result = load_runtime();
	if (result == vr::VRInitError_None) {
		result = g_core->Init(type, startup);
		if (result == vr::VRInitError_None) {
			g_token++;
		}
	}
	if (error) {
		*error = result;
	}
	return result == vr::VRInitError_None ? g_token : 0;
}

VR_INTERFACE uint32_t VR_CALLTYPE VR_InitInternal(vr::EVRInitError* error, vr::EVRApplicationType type)
{
	return VR_InitInternal2(error, type, nullptr);
}

VR_INTERFACE void VR_CALLTYPE VR_ShutdownInternal()
{
	if (g_core) {
		g_core->Cleanup();
		g_core = nullptr;
	}
}

VR_INTERFACE void* VR_CALLTYPE VR_GetGenericInterface(const char* version, vr::EVRInitError* error)
{
	if (!g_core) {
		if (error) {
			*error = vr::VRInitError_Init_NotInitialized;
		}
		return nullptr;
	}
	return g_core->GetGenericInterface(version, error);
}

VR_INTERFACE bool VR_CALLTYPE VR_IsInterfaceVersionValid(const char* version)
{
	return g_core && g_core->IsInterfaceVersionValid(version) == vr::VRInitError_None;
}

VR_INTERFACE uint32_t VR_CALLTYPE VR_GetInitToken()
{
	return g_token;
}

VR_INTERFACE bool VR_CALLTYPE VR_IsHmdPresent()
{
	return load_runtime() == vr::VRInitError_None && g_core->BIsHmdPresent();
}

VR_INTERFACE bool VR_CALLTYPE VR_IsRuntimeInstalled()
{
	std::string path;
	return read_runtime_path(vrpath_file(), path);
}

VR_INTERFACE bool VR_GetRuntimePath(char* buffer, uint32_t size, uint32_t* required)
{
	std::string path;
	if (!read_runtime_path(vrpath_file(), path)) {
		return false;
	}
	if (required) {
		*required = static_cast<uint32_t>(path.size() + 1);
	}
	if (size <= path.size()) {
		return false;
	}
	std::memcpy(buffer, path.c_str(), path.size() + 1);
	return true;
}

VR_INTERFACE const char* VR_CALLTYPE VR_GetVRInitErrorAsSymbol(vr::EVRInitError error)
{
	return g_core ? g_core->GetIDForVRInitError(error) : "VRInitError";
}

VR_INTERFACE const char* VR_CALLTYPE VR_GetVRInitErrorAsEnglishDescription(vr::EVRInitError error)
{
	return g_core ? g_core->GetEnglishStringForHmdError(error) : "VR runtime is not loaded";
}

namespace {

// A game that MSVC built calls a virtual method that returns a struct as (this, the address of the
// result, the arguments): the first argument after `this` is where the result goes. GCC passes the two
// the other way round, so calling the interface class from this program (built by GCC) would hide a
// mismatch in OpenComposite's vtable. This calls the way a game does, through the vtable slot.
template <class Result, class... Args>
Result* msvc_call(void* object, int slot, Result* out, Args... args)
{
	using Method = Result* (*)(void*, Result*, Args...);
	void** vtable = *reinterpret_cast<void***>(object);
	return reinterpret_cast<Method>(vtable[slot])(object, out, args...);
}

template <class T>
void release(T*& object)
{
	if (object) {
		object->Release();
		object = nullptr;
	}
}

bool finite_pose(const vr::HmdMatrix34_t& m)
{
	for (const auto& row : m.m) {
		for (const float v : row) {
			if (!std::isfinite(v)) {
				return false;
			}
		}
	}
	return true;
}


// === IVRInput: a manifest with default bindings, as a game ships it ===

const char* kActionManifest = R"JSON({
  "version": 1,
  "minimum_runtime_version": 1,
  "default_bindings": [
%BINDINGS%
  ],
  "actions": [
    { "name": "/actions/default/in/Grab", "type": "boolean", "requirement": "suggested" },
    { "name": "/actions/default/in/TriggerPull", "type": "vector1", "requirement": "suggested" },
    { "name": "/actions/default/in/TriggerClick", "type": "boolean", "requirement": "optional" },
    { "name": "/actions/default/in/Stick", "type": "vector2", "requirement": "optional" },
    { "name": "/actions/default/in/ButtonA", "type": "boolean", "requirement": "optional" },
    { "name": "/actions/default/in/Hand", "type": "pose", "requirement": "mandatory" },
    { "name": "/actions/default/out/Haptic", "type": "vibration", "requirement": "suggested" }
  ],
  "action_sets": [ { "name": "/actions/default", "usage": "leftright" } ],
  "localization": [ { "language_tag": "en_US", "/actions/default": "Default", "/actions/default/in/Grab": "Grab" } ]
})JSON";

// The sources of a binding file. Touch controllers name the A button and the thumbstick, the Vive wand a
// button on its trackpad ("click") and the trackpad.
std::string binding_file(const char* controller_type, bool touch)
{
	std::string text = std::string("{\n  \"controller_type\": \"") + controller_type + "\",\n  \"name\": \"cranvr smoke\",\n  \"description\": \"\",\n";
	text += "  \"bindings\": {\n    \"/actions/default\": {\n      \"sources\": [\n";
	text += R"JSON(        { "path": "/user/hand/left/input/grip", "mode": "button", "parameters": { "click_activate_threshold": "0.8", "click_deactivate_threshold": "0.7" },
          "inputs": { "click": { "output": "/actions/default/in/Grab" } } },
        { "path": "/user/hand/right/input/trigger", "mode": "trigger",
          "inputs": { "pull": { "output": "/actions/default/in/TriggerPull" }, "click": { "output": "/actions/default/in/TriggerClick" } } },
)JSON";
	if (touch) {
		text += R"JSON(        { "path": "/user/hand/right/input/thumbstick", "mode": "joystick", "inputs": { "position": { "output": "/actions/default/in/Stick" } } },
        { "path": "/user/hand/right/input/a", "mode": "button", "inputs": { "click": { "output": "/actions/default/in/ButtonA" } } }
)JSON";
	} else {
		text += R"JSON(        { "path": "/user/hand/right/input/trackpad", "mode": "trackpad", "inputs": { "position": { "output": "/actions/default/in/Stick" } } },
        { "path": "/user/hand/right/input/application_menu", "mode": "button", "inputs": { "click": { "output": "/actions/default/in/ButtonA" } } }
)JSON";
	}
	text += R"JSON(      ],
      "poses": [
        { "output": "/actions/default/in/Hand", "path": "/user/hand/left/pose/raw" },
        { "output": "/actions/default/in/Hand", "path": "/user/hand/right/pose/raw" }
      ],
      "haptics": [
        { "output": "/actions/default/out/Haptic", "path": "/user/hand/left/output/haptic" },
        { "output": "/actions/default/out/Haptic", "path": "/user/hand/right/output/haptic" }
      ]
    }
  }
}
)JSON";
	return text;
}

bool write_file(const std::string& path, const std::string& text)
{
	std::ofstream out(path, std::ios::binary);
	out << text;
	return static_cast<bool>(out);
}

// What the actions did over the run
struct ActionTest {
	vr::IVRInput* input = nullptr;
	vr::VRActionSetHandle_t set = 0;
	vr::VRActionHandle_t grab = 0, pull = 0, click = 0, stick = 0, button_a = 0, hand = 0, haptic = 0;
	vr::VRInputValueHandle_t left = 0, right = 0;
	bool touch = true;

	bool grab_seen = false, click_seen = false, a_seen = false, stick_seen = false;
	bool grab_active = false, pull_active = false, pose_active[2] = {};
	float pull_max = 0.0f, stick_max = 0.0f;
	uint64_t errors = 0, haptic_pulses = 0;
	vr::EVRInputError first_error = vr::VRInputError_None;
	float pose_x_min[2] = { 1e9f, 1e9f }, pose_x_max[2] = { -1e9f, -1e9f }, pose_y[2] = {}, pose_z[2] = {};
	uint64_t pose_valid[2] = {}, frames = 0;
	// The first frame with a valid pose, and how often the pose went from valid to not valid
	uint64_t pose_first[2] = { 0, 0 }, pose_flips[2] = {};
	bool pose_was_valid[2] = {};
	bool haptic_was_down = false;

	void note(vr::EVRInputError error)
	{
		if (error != vr::VRInputError_None) {
			errors++;
			if (first_error == vr::VRInputError_None) {
				first_error = error;
			}
		}
	}

	bool setup(const std::string& mode)
	{
		const bool with_touch = mode == "touch" || mode == "both";
		const bool with_vive = mode == "vive" || mode == "both";
		touch = with_touch;
		char temp[MAX_PATH] = {};
		GetTempPathA(sizeof(temp), temp);
		const std::string dir = std::string(temp) + "cranvr-openvr-actions";
		_mkdir(dir.c_str());
		std::string bindings;
		if (with_touch) {
			bindings += "    { \"controller_type\": \"oculus_touch\", \"binding_url\": \"bindings_oculus_touch.json\" }";
			write_file(dir + "\\bindings_oculus_touch.json", binding_file("oculus_touch", true));
		}
		if (with_vive) {
			bindings += std::string(bindings.empty() ? "" : ",\n") + "    { \"controller_type\": \"vive_controller\", \"binding_url\": \"bindings_vive_controller.json\" }";
			write_file(dir + "\\bindings_vive_controller.json", binding_file("vive_controller", false));
		}
		std::string manifest = kActionManifest;
		manifest.replace(manifest.find("%BINDINGS%"), 10, bindings);
		const std::string manifest_path = dir + "\\actions.json";
		if (!write_file(manifest_path, manifest)) {
			say("cannot write %s", manifest_path.c_str());
			return false;
		}
		input = vr::VRInput();
		if (!input) {
			say("no IVRInput");
			return false;
		}
		vr::EVRInputError error = input->SetActionManifestPath(manifest_path.c_str());
		say("SetActionManifestPath(%s): error %d", manifest_path.c_str(), static_cast<int>(error));
		if (error != vr::VRInputError_None) {
			return false;
		}
		const struct { const char* name; vr::VRActionHandle_t* handle; } actions[] = {
			{ "/actions/default/in/Grab", &grab }, { "/actions/default/in/TriggerPull", &pull }, { "/actions/default/in/TriggerClick", &click },
			{ "/actions/default/in/Stick", &stick }, { "/actions/default/in/ButtonA", &button_a }, { "/actions/default/in/Hand", &hand },
			{ "/actions/default/out/Haptic", &haptic },
		};
		for (const auto& action : actions) {
			error = input->GetActionHandle(action.name, action.handle);
			if (error != vr::VRInputError_None || *action.handle == vr::k_ulInvalidActionHandle) {
				say("GetActionHandle(%s): error %d", action.name, static_cast<int>(error));
				return false;
			}
		}
		if (input->GetActionSetHandle("/actions/default", &set) != vr::VRInputError_None || set == vr::k_ulInvalidActionSetHandle) {
			say("GetActionSetHandle failed");
			return false;
		}
		if (input->GetInputSourceHandle("/user/hand/left", &left) != vr::VRInputError_None || input->GetInputSourceHandle("/user/hand/right", &right) != vr::VRInputError_None) {
			say("GetInputSourceHandle failed");
			return false;
		}
		say("the manifest is loaded (%s bindings): 7 actions, 1 action set", mode.c_str());
		return true;
	}

	void frame(vr::TrackedDevicePose_t* poses)
	{
		(void)poses;
		frames++;
		vr::VRActiveActionSet_t active{};
		active.ulActionSet = set;
		note(input->UpdateActionState(&active, sizeof(active), 1));

		vr::InputDigitalActionData_t digital{};
		if (input->GetDigitalActionData(grab, &digital, sizeof(digital), vr::k_ulInvalidInputValueHandle) == vr::VRInputError_None) {
			grab_active |= digital.bActive;
			grab_seen |= digital.bActive && digital.bState;
			// The left grip drives a haptic pulse, as a game's "grab" feedback does
			if (digital.bActive && digital.bState && !haptic_was_down) {
				note(input->TriggerHapticVibrationAction(haptic, 0.0f, 0.1f, 160.0f, 0.8f, left));
				haptic_pulses++;
			}
			haptic_was_down = digital.bActive && digital.bState;
		} else {
			note(vr::VRInputError_InvalidHandle);
		}
		if (input->GetDigitalActionData(click, &digital, sizeof(digital), vr::k_ulInvalidInputValueHandle) == vr::VRInputError_None) {
			click_seen |= digital.bActive && digital.bState;
		}
		if (input->GetDigitalActionData(button_a, &digital, sizeof(digital), vr::k_ulInvalidInputValueHandle) == vr::VRInputError_None) {
			a_seen |= digital.bActive && digital.bState;
		}
		vr::InputAnalogActionData_t analog{};
		if (input->GetAnalogActionData(pull, &analog, sizeof(analog), vr::k_ulInvalidInputValueHandle) == vr::VRInputError_None) {
			pull_active |= analog.bActive;
			pull_max = std::max(pull_max, analog.x);
		}
		if (input->GetAnalogActionData(stick, &analog, sizeof(analog), vr::k_ulInvalidInputValueHandle) == vr::VRInputError_None) {
			stick_max = std::max(stick_max, analog.x);
			stick_seen |= analog.bActive && std::fabs(analog.x) > 0.3f;
		}
		const vr::VRInputValueHandle_t hands[2] = { left, right };
		for (int i = 0; i < 2; i++) {
			vr::InputPoseActionData_t pose{};
			if (input->GetPoseActionDataForNextFrame(hand, vr::TrackingUniverseStanding, &pose, sizeof(pose), hands[i]) == vr::VRInputError_None) {
				pose_active[i] |= pose.bActive;
				const bool valid_now = pose.bActive && pose.pose.bPoseIsValid;
				if (pose_was_valid[i] && !valid_now) {
					pose_flips[i]++;
				}
				pose_was_valid[i] = valid_now;
				if (pose.bActive && pose.pose.bPoseIsValid) {
					if (pose_valid[i] == 0) {
						pose_first[i] = frames;
					}
					pose_valid[i]++;
					const auto& m = pose.pose.mDeviceToAbsoluteTracking.m;
					pose_x_min[i] = std::min(pose_x_min[i], m[0][3]);
					pose_x_max[i] = std::max(pose_x_max[i], m[0][3]);
					pose_y[i] = m[1][3];
					pose_z[i] = m[2][3];
				}
			}
		}
	}

	// Returns the failure text, or nullptr
	const char* verdict(bool expect)
	{
		say("actions: %llu frames, %llu errors (first %d); grab (left grip) %s, trigger pull up to %.2f, trigger click %s, A %s, stick x up to %.2f%s; "
		    "haptic pulses sent %llu",
		    static_cast<unsigned long long>(frames), static_cast<unsigned long long>(errors), static_cast<int>(first_error), grab_seen ? "seen" : "never",
		    pull_max, click_seen ? "seen" : "never", a_seen ? "seen" : "never", stick_max, stick_seen ? "" : " (not seen)", static_cast<unsigned long long>(haptic_pulses));
		for (int i = 0; i < 2; i++) {
			say("pose of the %s hand: %s, %llu valid (first at frame %llu, %llu times lost), x from %.3f to %.3f, y %.3f, z %.3f", i == 0 ? "left" : "right",
			    pose_active[i] ? "active" : "inactive", static_cast<unsigned long long>(pose_valid[i]), static_cast<unsigned long long>(pose_first[i]),
			    static_cast<unsigned long long>(pose_flips[i]), pose_x_min[i], pose_x_max[i], pose_y[i], pose_z[i]);
		}
		if (errors) {
			return "an IVRInput call failed";
		}
		if (!expect) {
			return nullptr;
		}
		if (!pose_active[0] || !pose_active[1] || pose_valid[0] < frames / 2 || pose_valid[1] < frames / 2) {
			return "a hand's pose action was not active and valid";
		}
		// The probe's left grip stays at x -0.2 (the pose of /pose/raw sits a few centimeters off the grip) and the
		// right one swings 10 cm to each side of 0.25
		if (std::fabs(pose_x_min[0] - -0.2f) > 0.1f || pose_x_max[0] - pose_x_min[0] > 0.02f) {
			return "the left hand's pose is not where the probe holds it";
		}
		if (pose_x_max[1] - pose_x_min[1] < 0.15f) {
			return "the right hand's pose does not swing as the probe's does";
		}
		// The probe's hands are 1.1 to 1.2 m above its floor, which the host puts at height 0 (the probe's floor lies
		// 0.3 m below its room's origin, so the standing room puts the hands near 1.5 m)
		if (pose_y[0] < 1.2f || pose_y[0] > 1.7f || pose_y[1] < 1.2f || pose_y[1] > 1.7f) {
			return "the hands are not at the height that the probe's script and floor give";
		}
		if (!grab_active || !pull_active) {
			return "the grab or the trigger action never became active";
		}
		if (!grab_seen) {
			return "the grab action (left grip click) never fired";
		}
		if (pull_max < 0.7f || pull_max > 0.8f) {
			return "the trigger pull action did not reach the scripted 0.75";
		}
		if (!click_seen) {
			return "the trigger click action never fired";
		}
		if (!stick_seen || stick_max < 0.4f || stick_max > 0.6f) {
			return "the stick or trackpad action did not reach the scripted 0.5";
		}
		if (touch && !a_seen) {
			return "the A button action never fired";
		}
		return nullptr;
	}
};

} // namespace

int main(int argc, char** argv)
{
	double seconds = 15;
	bool expect_input = false, load_only = false, no_input = false;
	std::string actions_mode;
	DXGI_FORMAT texture_format = DXGI_FORMAT_R8G8B8A8_UNORM, view_format = DXGI_FORMAT_R8G8B8A8_UNORM;
	for (int i = 1; i < argc; i++) {
		if (!std::strcmp(argv[i], "--seconds") && i + 1 < argc) {
			seconds = std::atof(argv[++i]);
		} else if (!std::strcmp(argv[i], "--expect-input")) {
			expect_input = true;
		} else if (!std::strcmp(argv[i], "--actions") && i + 1 < argc) {
			actions_mode = argv[++i];
			if (actions_mode != "touch" && actions_mode != "vive" && actions_mode != "both") {
				std::fprintf(stderr, "--actions takes touch, vive or both\n");
				return 2;
			}
		} else if (!std::strcmp(argv[i], "--no-input")) {
			no_input = true;
		} else if (!std::strcmp(argv[i], "--load-only")) {
			load_only = true;
		} else if (!std::strcmp(argv[i], "--format") && i + 1 < argc) {
			const char* name = argv[++i];
			if (!std::strcmp(name, "rgba")) {
				texture_format = view_format = DXGI_FORMAT_R8G8B8A8_UNORM;
			} else if (!std::strcmp(name, "bgra")) {
				texture_format = view_format = DXGI_FORMAT_B8G8R8A8_UNORM;
			} else if (!std::strcmp(name, "bgra-typeless")) {
				texture_format = DXGI_FORMAT_B8G8R8A8_TYPELESS;
				view_format = DXGI_FORMAT_B8G8R8A8_UNORM;
			} else {
				std::fprintf(stderr, "--format takes rgba, bgra or bgra-typeless\n");
				return 2;
			}
		} else if (!std::strcmp(argv[i], "--runtime") && i + 1 < argc) {
			g_runtime_path = argv[++i];
			g_runtime_given = true;
		} else {
			std::fprintf(stderr, "usage: openvr_smoke.exe [--seconds N] [--expect-input] [--no-input] [--actions MODE] [--format F] [--load-only --runtime DIR]\n");
			return 2;
		}
	}

	// The legacy controller state would load OpenComposite's own empty manifest: with a manifest of the program's own
	// the legacy reads stay out
	if (!actions_mode.empty()) {
		no_input = true;
	}
	if (load_only) {
		if (!g_runtime_given) {
			return fail("--load-only needs --runtime");
		}
		if (load_runtime() != vr::VRInitError_None) {
			return fail("the runtime does not load");
		}
		say("PASS");
		return 0;
	}

	if (!vr::VR_IsRuntimeInstalled()) {
		return fail("no OpenVR runtime in the path file");
	}
	vr::EVRInitError error = vr::VRInitError_None;
	vr::IVRSystem* system = vr::VR_Init(&error, vr::VRApplication_Scene);
	if (error != vr::VRInitError_None || !system) {
		say("VR_Init failed: %s", vr::VR_GetVRInitErrorAsSymbol(error));
		return fail("VR_Init");
	}
	vr::IVRCompositor* compositor = vr::VRCompositor();
	if (!compositor) {
		return fail("no compositor");
	}
	say("initialised, runtime %s", g_runtime_path.c_str());
	ActionTest actions;
	if (!actions_mode.empty() && !actions.setup(actions_mode)) {
		return fail("the action manifest did not load");
	}

	char text[256] = {};
	vr::ETrackedPropertyError property = vr::TrackedProp_Success;
	system->GetStringTrackedDeviceProperty(vr::k_unTrackedDeviceIndex_Hmd, vr::Prop_ManufacturerName_String, text, sizeof(text), &property);
	say("head: %s (property error %d)", text, property);
	text[0] = 0;
	system->GetStringTrackedDeviceProperty(vr::k_unTrackedDeviceIndex_Hmd, vr::Prop_ModelNumber_String, text, sizeof(text), &property);
	say("head model: %s", text);

	uint32_t width = 0, height = 0;
	system->GetRecommendedRenderTargetSize(&width, &height);
	say("recommended render target %ux%u, eye textures of DXGI format %d", width, height, static_cast<int>(texture_format));
	if (width < 256 || height < 256 || width > 8192 || height > 8192) {
		return fail("the recommended render target size is not sane");
	}
	// Slots of IVRSystem_022: GetProjectionMatrix 1, GetEyeToHeadTransform 4, GetHiddenAreaMesh 32
	vr::HmdMatrix44_t projection{};
	vr::HmdMatrix34_t eye{};
	vr::HiddenAreaMesh_t mesh{ reinterpret_cast<vr::HmdVector2_t*>(1), 7 };
	msvc_call(system, 1, &projection, vr::Eye_Left, 0.1f, 100.0f);
	msvc_call(system, 4, &eye, vr::Eye_Right);
	msvc_call(system, 32, &mesh, vr::Eye_Left, vr::k_eHiddenAreaMesh_Standard);
	if (!std::isfinite(projection.m[0][0]) || projection.m[0][0] == 0.0f || !finite_pose(eye)) {
		return fail("the projection or the eye transform is not sane");
	}
	say("right eye is %.1f mm from the head's middle, the hidden area has %u triangles", eye.m[0][3] * 1000.0f, mesh.unTriangleCount);
	if (mesh.unTriangleCount != 0 || mesh.pVertexData != nullptr) {
		// The runtime of cranvr has no visibility mask: the answer is the empty mesh
		say("note: the runtime gives a hidden area mesh");
	}
	// The same through the FnTable, whose functions return the struct by value
	vr::EVRInitError table_error = vr::VRInitError_None;
	void** table = static_cast<void**>(VR_GetGenericInterface("FnTable:IVRSystem_022", &table_error));
	if (!table || table_error != vr::VRInitError_None) {
		return fail("no FnTable of IVRSystem");
	}
	// The table lists the functions in the order of the methods: GetEyeToHeadTransform is the fifth
	const auto table_eye_function = reinterpret_cast<vr::HmdMatrix34_t (*)(vr::EVREye)>(table[4]);
	const vr::HmdMatrix34_t table_eye = table_eye_function(vr::Eye_Right);
	if (std::memcmp(&table_eye, &eye, sizeof(eye)) != 0) {
		return fail("the FnTable and the vtable give different eye transforms");
	}

	// The device that a game makes: the first adapter, as the OpenXR runtime's own test does
	IDXGIFactory1* factory = nullptr;
	if (FAILED(CreateDXGIFactory1(__uuidof(IDXGIFactory1), reinterpret_cast<void**>(&factory)))) {
		return fail("CreateDXGIFactory1");
	}
	IDXGIAdapter1* adapter = nullptr;
	HRESULT hr = factory->EnumAdapters1(0, &adapter);
	release(factory);
	if (FAILED(hr)) {
		return fail("EnumAdapters1");
	}
	const D3D_FEATURE_LEVEL levels[] = { D3D_FEATURE_LEVEL_11_0 };
	ID3D11Device* device = nullptr;
	ID3D11DeviceContext* context = nullptr;
	hr = D3D11CreateDevice(adapter, D3D_DRIVER_TYPE_UNKNOWN, nullptr, D3D11_CREATE_DEVICE_BGRA_SUPPORT, levels, 1, D3D11_SDK_VERSION, &device, nullptr, &context);
	release(adapter);
	if (FAILED(hr)) {
		return fail("D3D11CreateDevice");
	}

	ID3D11Texture2D* targets[2] = {};
	ID3D11RenderTargetView* views[2] = {};
	for (int i = 0; i < 2; i++) {
		D3D11_TEXTURE2D_DESC desc{};
		desc.Width = width;
		desc.Height = height;
		desc.MipLevels = 1;
		desc.ArraySize = 1;
		desc.Format = texture_format;
		desc.SampleDesc.Count = 1;
		desc.Usage = D3D11_USAGE_DEFAULT;
		desc.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
		D3D11_RENDER_TARGET_VIEW_DESC view_desc{};
		view_desc.Format = view_format;
		view_desc.ViewDimension = D3D11_RTV_DIMENSION_TEXTURE2D;
		if (FAILED(device->CreateTexture2D(&desc, nullptr, &targets[i])) || FAILED(device->CreateRenderTargetView(targets[i], &view_desc, &views[i]))) {
			return fail("the render target of an eye");
		}
	}

	using Clock = std::chrono::steady_clock;
	const auto begin = Clock::now();
	uint64_t frames = 0, valid_poses = 0, submit_errors = 0, wait_errors = 0;
	float max_right_trigger = 0.0f, max_left_grip_pulled = 0.0f, a_seen = 0.0f;
	// Once the frames flow the views come from the headset: the eyes sit apart. Read while the host's
	// client is still there, once a second, and keep the last answer that is not the zero of "no views yet".
	float eye_distance = 0.0f;
	Clock::time_point last_eye_read = begin;
	bool left_connected = false, right_connected = false, left_grip_pressed = false, a_pressed = false;
	vr::TrackedDevicePose_t poses[vr::k_unMaxTrackedDeviceCount];
	vr::EVRCompositorError first_error = vr::VRCompositorError_None;
	// Where a frame's time goes, in seconds over the run
	double wait_time = 0, draw_time = 0, submit_time = 0, handoff_time = 0, input_time = 0;
	// The time between two returns of WaitGetPoses, in milliseconds
	std::vector<double> intervals;
	// The frames that took far longer than a period, as (seconds since the start, milliseconds)
	std::vector<std::pair<double, double>> stalls;
	// Frames in each second of the run
	std::vector<unsigned> per_second;
	Clock::time_point last_return{};
	const auto since = [](Clock::time_point& from) {
		const auto now = Clock::now();
		const double spent = std::chrono::duration<double>(now - from).count();
		from = now;
		return spent;
	};

	while (std::chrono::duration<double>(Clock::now() - begin).count() < seconds) {
		Clock::time_point mark = Clock::now();
		const vr::EVRCompositorError wait = compositor->WaitGetPoses(poses, vr::k_unMaxTrackedDeviceCount, nullptr, 0);
		wait_time += since(mark);
		if (last_return != Clock::time_point{}) {
			intervals.push_back(1000.0 * std::chrono::duration<double>(mark - last_return).count());
			if (intervals.back() > 40.0) {
				stalls.emplace_back(std::chrono::duration<double>(mark - begin).count(), intervals.back());
			}
		}
		last_return = mark;
		{
			const size_t second = static_cast<size_t>(std::chrono::duration<double>(mark - begin).count());
			if (per_second.size() <= second) {
				per_second.resize(second + 1, 0);
			}
			per_second[second]++;
		}
		if (wait != vr::VRCompositorError_None) {
			wait_errors++;
			if (first_error == vr::VRCompositorError_None) {
				first_error = wait;
			}
		}
		if (poses[vr::k_unTrackedDeviceIndex_Hmd].bPoseIsValid && poses[vr::k_unTrackedDeviceIndex_Hmd].bDeviceIsConnected
		    && finite_pose(poses[vr::k_unTrackedDeviceIndex_Hmd].mDeviceToAbsoluteTracking)) {
			valid_poses++;
		}

		// The two eyes differ, and each frame differs from the one before, so a frame that the
		// host shows is a frame of this program
		const float shade = static_cast<float>(frames % 120) / 120.0f;
		const float left_color[4] = { 0.1f, 0.2f + 0.6f * shade, 0.9f, 1.0f };
		const float right_color[4] = { 0.9f, 0.2f + 0.6f * shade, 0.1f, 1.0f };
		context->ClearRenderTargetView(views[0], left_color);
		context->ClearRenderTargetView(views[1], right_color);
		context->Flush();
		draw_time += since(mark);

		for (int i = 0; i < 2; i++) {
			vr::Texture_t texture{ targets[i], vr::TextureType_DirectX, vr::ColorSpace_Gamma };
			const vr::EVRCompositorError submit = compositor->Submit(i == 0 ? vr::Eye_Left : vr::Eye_Right, &texture);
			if (submit != vr::VRCompositorError_None) {
				submit_errors++;
				if (first_error == vr::VRCompositorError_None) {
					first_error = submit;
				}
			}
		}
		submit_time += since(mark);
		compositor->PostPresentHandoff();
		handoff_time += since(mark);
		frames++;
		if (!actions_mode.empty()) {
			actions.frame(poses);
		}

		// The scripted controllers of cranvr-probe: the legacy state of OpenVR
		const vr::TrackedDeviceIndex_t left = system->GetTrackedDeviceIndexForControllerRole(vr::TrackedControllerRole_LeftHand);
		const vr::TrackedDeviceIndex_t right = system->GetTrackedDeviceIndexForControllerRole(vr::TrackedControllerRole_RightHand);
		left_connected |= left != vr::k_unTrackedDeviceIndexInvalid && poses[left].bDeviceIsConnected && poses[left].bPoseIsValid;
		right_connected |= right != vr::k_unTrackedDeviceIndexInvalid && poses[right].bDeviceIsConnected && poses[right].bPoseIsValid;
		vr::VRControllerState_t state{};
		if (!no_input && left != vr::k_unTrackedDeviceIndexInvalid && system->GetControllerState(left, &state, sizeof(state))) {
			left_grip_pressed |= (state.ulButtonPressed & vr::ButtonMaskFromId(vr::k_EButton_Grip)) != 0;
			max_left_grip_pulled = std::max(max_left_grip_pulled, state.rAxis[2].x);
		}
		if (!no_input && right != vr::k_unTrackedDeviceIndexInvalid && system->GetControllerState(right, &state, sizeof(state))) {
			max_right_trigger = std::max(max_right_trigger, state.rAxis[1].x);
			a_pressed |= (state.ulButtonPressed & vr::ButtonMaskFromId(vr::k_EButton_A)) != 0;
			a_seen = std::max(a_seen, a_pressed ? 1.0f : 0.0f);
		}

		if (std::chrono::duration<double>(Clock::now() - last_eye_read).count() >= 1.0) {
			last_eye_read = Clock::now();
			vr::HmdMatrix34_t left_eye{}, right_eye{};
			msvc_call(system, 4, &left_eye, vr::Eye_Left);
			msvc_call(system, 4, &right_eye, vr::Eye_Right);
			if (right_eye.m[0][3] - left_eye.m[0][3] != 0.0f) {
				eye_distance = right_eye.m[0][3] - left_eye.m[0][3];
			}
		}

		vr::VREvent_t event;
		while (system->PollNextEvent(&event, sizeof(event))) {
			if (event.eventType == vr::VREvent_Quit) {
				say("the runtime asks the program to quit");
				seconds = 0;
			}
		}
		input_time += since(mark);
	}

	const double elapsed = std::chrono::duration<double>(Clock::now() - begin).count();
	say("%llu frames in %.1f s (%.1f per second), %llu valid head poses, %llu wait errors, %llu submit errors (first %d)",
	    static_cast<unsigned long long>(frames), elapsed, static_cast<double>(frames) / elapsed, static_cast<unsigned long long>(valid_poses),
	    static_cast<unsigned long long>(wait_errors), static_cast<unsigned long long>(submit_errors), static_cast<int>(first_error));
	say("per frame: wait %.2f ms, draw %.2f ms, submit %.2f ms, handoff %.2f ms, input %.2f ms", 1000 * wait_time / frames, 1000 * draw_time / frames,
	    1000 * submit_time / frames, 1000 * handoff_time / frames, 1000 * input_time / frames);
	if (intervals.size() > 10) {
		// The refresh rate that the runtime reports: a frame that lasts 1.5 periods or more missed a slot
		float hz = system->GetFloatTrackedDeviceProperty(vr::k_unTrackedDeviceIndex_Hmd, vr::Prop_DisplayFrequency_Float, &property);
		if (property != vr::TrackedProp_Success || hz < 1.0f) {
			hz = 120.0f;
		}
		const double period = 1000.0 / hz;
		std::vector<double> sorted = intervals;
		std::sort(sorted.begin(), sorted.end());
		const auto percentile = [&](double q) { return sorted[static_cast<size_t>(q * static_cast<double>(sorted.size() - 1))]; };
		size_t slots[4] = {}; // frames that took one, two, three, more periods
		for (const double ms : intervals) {
			slots[std::min<size_t>(3, static_cast<size_t>(std::max(0.0, ms / period - 0.5)))]++;
		}
		const double total = static_cast<double>(intervals.size());
		say("frame intervals at %.1f Hz (period %.2f ms): median %.2f, p90 %.2f, p99 %.2f, max %.2f ms; frames of one period %.1f%%, two %.1f%%, three %.1f%%, more %.1f%%",
		    hz, period, percentile(0.5), percentile(0.9), percentile(0.99), sorted.back(), 100.0 * slots[0] / total, 100.0 * slots[1] / total,
		    100.0 * slots[2] / total, 100.0 * slots[3] / total);
	}
	if (!per_second.empty()) {
		std::string line = "frames in each second:";
		for (const unsigned n : per_second) {
			line += " " + std::to_string(n);
		}
		say("%s", line.c_str());
		// The rate once the start has settled: the second after the third, to the last full second
		if (per_second.size() > 5) {
			unsigned sum = 0;
			for (size_t i = 3; i + 1 < per_second.size(); i++) {
				sum += per_second[i];
			}
			say("after the start (seconds 3 to %llu): %.1f frames per second", static_cast<unsigned long long>(per_second.size() - 2),
			    static_cast<double>(sum) / static_cast<double>(per_second.size() - 4));
		}
	}
	for (size_t i = 0; i < stalls.size() && i < 12; i++) {
		say("a frame took %.0f ms at %.2f s", stalls[i].second, stalls[i].first);
	}
	if (stalls.size() > 12) {
		say("and %llu more frames of more than 40 ms", static_cast<unsigned long long>(stalls.size() - 12));
	}
	say("controllers: left %s, right %s, right trigger up to %.2f, left grip %s (axis up to %.2f), A %s", left_connected ? "tracked" : "missing",
	    right_connected ? "tracked" : "missing", max_right_trigger, left_grip_pressed ? "pressed" : "not pressed", max_left_grip_pulled,
	    a_pressed ? "pressed" : "not pressed");

	say("the eyes are %.1f mm apart", eye_distance * 1000.0f);

	for (int i = 0; i < 2; i++) {
		release(views[i]);
		release(targets[i]);
	}
	release(context);
	release(device);
	vr::VR_Shutdown();

	if (frames < 30) {
		return fail("fewer than 30 frames");
	}
	if (wait_errors || submit_errors) {
		return fail("the compositor reported errors");
	}
	if (valid_poses < frames * 9 / 10) {
		return fail("the head pose was not valid in 90 percent of the frames");
	}
	if (!(eye_distance > 0.03f && eye_distance < 0.09f)) {
		return fail("the eyes are not 3 to 9 cm apart");
	}
	if (!actions_mode.empty()) {
		if (const char* problem = actions.verdict(expect_input)) {
			return fail(problem);
		}
	}
	if (expect_input && actions_mode.empty()) {
		if (!left_connected || !right_connected) {
			return fail("a controller was not tracked");
		}
		if (max_right_trigger < 0.7f || max_right_trigger > 0.8f) {
			return fail("the right trigger did not reach the scripted 0.75");
		}
		if (!left_grip_pressed) {
			return fail("the left grip was never pressed");
		}
		if (!a_pressed) {
			return fail("the A button was never pressed");
		}
	}
	say("PASS");
	return 0;
}
