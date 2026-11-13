/* A minimal Vamp analyser: one parameter (Gain), one program, and one output whose feature is
   the mean absolute value of the first channel, scaled by the gain. API version 2. */
#include <stdlib.h>

#ifdef _WIN32
#define VAMP_EXPORT __declspec(dllexport)
#else
#define VAMP_EXPORT
#endif

typedef void *VampPluginHandle;

typedef enum {
    vampOneSamplePerStep,
    vampFixedSampleRate,
    vampVariableSampleRate
} VampSampleType;

typedef enum {
    vampTimeDomain,
    vampFrequencyDomain
} VampInputDomain;

typedef struct {
    const char *identifier;
    const char *name;
    const char *description;
    const char *unit;
    float minValue;
    float maxValue;
    float defaultValue;
    int isQuantized;
    float quantizeStep;
    const char **valueNames;
} VampParameterDescriptor;

typedef struct {
    const char *identifier;
    const char *name;
    const char *description;
    const char *unit;
    int hasFixedBinCount;
    unsigned int binCount;
    const char **binNames;
    int hasKnownExtents;
    float minValue;
    float maxValue;
    int isQuantized;
    float quantizeStep;
    VampSampleType sampleType;
    float sampleRate;
    int hasDuration;
} VampOutputDescriptor;

typedef struct {
    int hasTimestamp;
    int sec;
    int nsec;
    unsigned int valueCount;
    float *values;
    char *label;
} VampFeature;

typedef struct {
    int hasDuration;
    int durationSec;
    int durationNsec;
} VampFeatureV2;

typedef union {
    VampFeature v1;
    VampFeatureV2 v2;
} VampFeatureUnion;

typedef struct {
    unsigned int featureCount;
    VampFeatureUnion *features;
} VampFeatureList;

typedef struct _VampPluginDescriptor {
    unsigned int vampApiVersion;
    const char *identifier;
    const char *name;
    const char *description;
    const char *maker;
    int pluginVersion;
    const char *copyright;
    unsigned int parameterCount;
    const VampParameterDescriptor **parameters;
    unsigned int programCount;
    const char **programs;
    VampInputDomain inputDomain;
    VampPluginHandle (*instantiate)(const struct _VampPluginDescriptor *, float);
    void (*cleanup)(VampPluginHandle);
    int (*initialise)(VampPluginHandle, unsigned int, unsigned int, unsigned int);
    void (*reset)(VampPluginHandle);
    float (*getParameter)(VampPluginHandle, int);
    void (*setParameter)(VampPluginHandle, int, float);
    unsigned int (*getCurrentProgram)(VampPluginHandle);
    void (*selectProgram)(VampPluginHandle, unsigned int);
    unsigned int (*getPreferredStepSize)(VampPluginHandle);
    unsigned int (*getPreferredBlockSize)(VampPluginHandle);
    unsigned int (*getMinChannelCount)(VampPluginHandle);
    unsigned int (*getMaxChannelCount)(VampPluginHandle);
    unsigned int (*getOutputCount)(VampPluginHandle);
    VampOutputDescriptor *(*getOutputDescriptor)(VampPluginHandle, unsigned int);
    void (*releaseOutputDescriptor)(VampOutputDescriptor *);
    VampFeatureList *(*process)(VampPluginHandle, const float *const *, int, int);
    VampFeatureList *(*getRemainingFeatures)(VampPluginHandle);
    void (*releaseFeatureSet)(VampFeatureList *);
} VampPluginDescriptor;

typedef struct {
    float sampleRate;
    unsigned int channels;
    unsigned int step;
    unsigned int block;
    int initialized;
    float gain;
} RmsInstance;

static VampPluginHandle rms_instantiate(const VampPluginDescriptor *descriptor, float rate) {
    RmsInstance *instance = (RmsInstance *)calloc(1, sizeof(RmsInstance));
    (void)descriptor;
    if (instance) {
        instance->sampleRate = rate;
        instance->gain = 1.0f;
    }
    return instance;
}

static void rms_cleanup(VampPluginHandle handle) {
    free(handle);
}

static int rms_initialise(VampPluginHandle handle, unsigned int channels, unsigned int step,
                          unsigned int block) {
    RmsInstance *instance = (RmsInstance *)handle;
    if (channels < 1 || channels > 2 || step == 0 || block == 0) return 0;
    instance->channels = channels;
    instance->step = step;
    instance->block = block;
    instance->initialized = 1;
    return 1;
}

static void rms_reset(VampPluginHandle handle) {
    RmsInstance *instance = (RmsInstance *)handle;
    instance->initialized = 1;
}

static float rms_get_parameter(VampPluginHandle handle, int index) {
    RmsInstance *instance = (RmsInstance *)handle;
    return index == 0 ? instance->gain : 0.0f;
}

static void rms_set_parameter(VampPluginHandle handle, int index, float value) {
    RmsInstance *instance = (RmsInstance *)handle;
    if (index == 0) instance->gain = value;
}

static unsigned int rms_current_program(VampPluginHandle handle) {
    (void)handle;
    return 0;
}

static void rms_select_program(VampPluginHandle handle, unsigned int program) {
    (void)handle;
    (void)program;
}

static unsigned int rms_preferred_step(VampPluginHandle handle) {
    (void)handle;
    return 0; /* the host picks one */
}

static unsigned int rms_preferred_block(VampPluginHandle handle) {
    (void)handle;
    return 1024;
}

static unsigned int rms_min_channels(VampPluginHandle handle) {
    (void)handle;
    return 1;
}

static unsigned int rms_max_channels(VampPluginHandle handle) {
    (void)handle;
    return 2;
}

static unsigned int rms_output_count(VampPluginHandle handle) {
    (void)handle;
    return 1;
}

static const char *rms_bin_names[1] = {"rms"};

static VampOutputDescriptor *rms_output_descriptor(VampPluginHandle handle, unsigned int index) {
    VampOutputDescriptor *output;
    (void)handle;
    if (index != 0) return 0;
    output = (VampOutputDescriptor *)calloc(1, sizeof(VampOutputDescriptor));
    if (!output) return 0;
    output->identifier = "rms";
    output->name = "RMS";
    output->description = "Mean absolute level";
    output->unit = "";
    output->hasFixedBinCount = 1;
    output->binCount = 1;
    output->binNames = rms_bin_names;
    output->hasKnownExtents = 1;
    output->minValue = 0.0f;
    output->maxValue = 10.0f;
    output->isQuantized = 0;
    output->quantizeStep = 0.0f;
    output->sampleType = vampOneSamplePerStep;
    output->sampleRate = 0.0f;
    output->hasDuration = 0;
    return output;
}

static void rms_release_output_descriptor(VampOutputDescriptor *descriptor) {
    free(descriptor);
}

static VampFeatureList *rms_process(VampPluginHandle handle, const float *const *buffers,
                                    int sec, int nsec) {
    RmsInstance *instance = (RmsInstance *)handle;
    VampFeatureList *list;
    unsigned int i;
    float sum = 0.0f;
    (void)sec;
    (void)nsec;
    if (!instance->initialized || !buffers || !buffers[0]) return 0;
    for (i = 0; i < instance->block; ++i) {
        float sample = buffers[0][i];
        sum += sample < 0.0f ? -sample : sample;
    }
    list = (VampFeatureList *)calloc(1, sizeof(VampFeatureList));
    if (!list) return 0;
    list->featureCount = 1;
    /* API version 2 feature lists carry two unions per feature; the second may hold a
       duration. `calloc` leaves it empty, which means "no duration". */
    list->features = (VampFeatureUnion *)calloc(2, sizeof(VampFeatureUnion));
    if (!list->features) {
        free(list);
        return 0;
    }
    list->features[0].v1.hasTimestamp = 0;
    list->features[0].v1.valueCount = 1;
    list->features[0].v1.values = (float *)malloc(sizeof(float));
    if (!list->features[0].v1.values) {
        free(list->features);
        free(list);
        return 0;
    }
    list->features[0].v1.values[0] = (sum / (float)instance->block) * instance->gain;
    list->features[1].v2.hasDuration = 0;
    return list;
}

static VampFeatureList *rms_remaining(VampPluginHandle handle) {
    (void)handle;
    return 0;
}

static void rms_release_features(VampFeatureList *list) {
    unsigned int i;
    if (!list) return;
    if (list->features) {
        for (i = 0; i < list->featureCount; ++i) {
            free(list->features[i].v1.values);
            free(list->features[i].v1.label);
        }
        free(list->features);
    }
    free(list);
}

static const VampParameterDescriptor gain_parameter = {
    "gain", "Gain", "Scales the reported level", "", 0.0f, 2.0f, 1.0f, 0, 0.0f, 0,
};
static const VampParameterDescriptor *parameter_table[1] = {&gain_parameter};
static const char *program_names[1] = {"Default"};

static const VampPluginDescriptor rms_descriptor = {
    2u,
    "vamp_test_rms",
    "Test RMS",
    "Mean absolute level",
    "Pro Wave",
    1,
    "",
    1u,
    parameter_table,
    1u,
    program_names,
    vampTimeDomain,
    rms_instantiate,
    rms_cleanup,
    rms_initialise,
    rms_reset,
    rms_get_parameter,
    rms_set_parameter,
    rms_current_program,
    rms_select_program,
    rms_preferred_step,
    rms_preferred_block,
    rms_min_channels,
    rms_max_channels,
    rms_output_count,
    rms_output_descriptor,
    rms_release_output_descriptor,
    rms_process,
    rms_remaining,
    rms_release_features,
};

VAMP_EXPORT const VampPluginDescriptor *vampGetPluginDescriptor(unsigned int api_version,
                                                                 unsigned int index) {
    (void)api_version;
    return index == 0 ? &rms_descriptor : 0;
}
