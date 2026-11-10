/* A minimal LADSPA plug-in: one audio input, one audio output and a Gain control. */
#include <stdlib.h>

#ifdef _WIN32
#define LADSPA_EXPORT __declspec(dllexport)
#else
#define LADSPA_EXPORT
#endif

typedef float LADSPA_Data;
typedef unsigned long LADSPA_PortDescriptor;

typedef struct {
    int hint;
    LADSPA_Data lower;
    LADSPA_Data upper;
} LADSPA_PortRangeHint;

typedef struct _LADSPA_Descriptor LADSPA_Descriptor;
typedef void *LADSPA_Handle;

struct _LADSPA_Descriptor {
    unsigned long unique_id;
    const char *label;
    int properties;
    const char *name;
    const char *maker;
    const char *copyright;
    unsigned long port_count;
    const LADSPA_PortDescriptor *port_descriptors;
    const char *const *port_names;
    const LADSPA_PortRangeHint *port_range_hints;
    void *implementation_data;
    LADSPA_Handle (*instantiate)(const LADSPA_Descriptor *, unsigned long);
    void (*connect_port)(LADSPA_Handle, unsigned long, LADSPA_Data *);
    void (*activate)(LADSPA_Handle);
    void (*run)(LADSPA_Handle, unsigned long);
    void (*run_adding)(LADSPA_Handle, unsigned long);
    void (*set_run_adding_gain)(LADSPA_Handle, LADSPA_Data);
    void (*deactivate)(LADSPA_Handle);
    void (*cleanup)(LADSPA_Handle);
};

typedef struct {
    const LADSPA_Data *input;
    LADSPA_Data *output;
    LADSPA_Data *gain;
} GainInstance;

static LADSPA_Handle gain_instantiate(const LADSPA_Descriptor *descriptor, unsigned long rate) {
    (void)descriptor;
    (void)rate;
    GainInstance *instance = (GainInstance *)calloc(1, sizeof(GainInstance));
    if (instance) instance->gain = 0;
    return instance;
}

static void gain_connect(LADSPA_Handle handle, unsigned long port, LADSPA_Data *data) {
    GainInstance *instance = (GainInstance *)handle;
    if (port == 0) instance->input = data;
    else if (port == 1) instance->output = data;
    else if (port == 2) instance->gain = data;
}

static void gain_run(LADSPA_Handle handle, unsigned long frames) {
    GainInstance *instance = (GainInstance *)handle;
    const LADSPA_Data gain = instance->gain ? *instance->gain : 0;
    for (unsigned long i = 0; i < frames; ++i) {
        instance->output[i] = instance->input[i] * gain;
    }
}

static void gain_cleanup(LADSPA_Handle handle) {
    free(handle);
}

static const LADSPA_PortDescriptor ports[3] = {
    0x1 | 0x8, /* input, audio */
    0x2 | 0x8, /* output, audio */
    0x1 | 0x4, /* input, control */
};
static const char *const names[3] = {"Input", "Output", "Gain"};
static const LADSPA_PortRangeHint hints[3] = {
    {0, 0.0f, 0.0f},
    {0, 0.0f, 0.0f},
    {0x1 | 0x2, 0.0f, 1.0f}, /* bounded below and above */
};

static const LADSPA_Descriptor descriptor = {
    1234567u, "gain", 0, "Test Gain", "Pro Wave", "", 3, ports, names, hints, 0,
    gain_instantiate, gain_connect, 0, gain_run, 0, 0, 0, gain_cleanup,
};

LADSPA_EXPORT const LADSPA_Descriptor *ladspa_descriptor(unsigned long index) {
    return index == 0 ? &descriptor : 0;
}
