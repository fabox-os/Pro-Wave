/* A tiny CLAP audio effect for the host's tests: a stereo gain with one parameter ("Gain",
 * 0..2, default 1), a latency of 7 samples (reported, not applied) and a state of one double.
 * The structures are CLAP 1.2's, written out so the test needs no SDK. */
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#if defined(_WIN32)
#define EXPORT __declspec(dllexport)
#else
#define EXPORT __attribute__((visibility("default")))
#endif

typedef struct { uint32_t major, minor, revision; } version_t;
typedef struct {
    version_t version;
    const char *id, *name, *vendor, *url, *manual_url, *support_url, *plugin_version, *description;
    const char *const *features;
} descriptor_t;
typedef struct host host_t;
typedef struct { uint32_t size, time; uint16_t space, type; uint32_t flags; } event_header_t;
typedef struct {
    event_header_t header;
    uint32_t param_id;
    void *cookie;
    int32_t note_id;
    int16_t port, channel, key;
    double value;
} event_param_t;
typedef struct in_events {
    void *ctx;
    uint32_t (*size)(const struct in_events *);
    const event_header_t *(*get)(const struct in_events *, uint32_t);
} in_events_t;
typedef struct {
    float **data32;
    double **data64;
    uint32_t channel_count, latency;
    uint64_t constant_mask;
} audio_buffer_t;
typedef struct {
    int64_t steady_time;
    uint32_t frames;
    const void *transport;
    const audio_buffer_t *inputs;
    audio_buffer_t *outputs;
    uint32_t input_count, output_count;
    const in_events_t *in_events;
    const void *out_events;
} process_t;
typedef struct plugin {
    const descriptor_t *desc;
    void *data;
    bool (*init)(const struct plugin *);
    void (*destroy)(const struct plugin *);
    bool (*activate)(const struct plugin *, double, uint32_t, uint32_t);
    void (*deactivate)(const struct plugin *);
    bool (*start_processing)(const struct plugin *);
    void (*stop_processing)(const struct plugin *);
    void (*reset)(const struct plugin *);
    int32_t (*process)(const struct plugin *, const process_t *);
    const void *(*get_extension)(const struct plugin *, const char *);
    void (*on_main_thread)(const struct plugin *);
} plugin_t;
typedef struct {
    uint32_t id, flags;
    void *cookie;
    char name[256], module[1024];
    double min, max, def;
} param_info_t;
typedef struct {
    uint32_t id;
    char name[256];
    uint32_t flags, channel_count;
    const char *port_type;
    uint32_t in_place_pair;
} port_info_t;
typedef struct ostream { void *ctx; int64_t (*write)(const struct ostream *, const void *, uint64_t); } ostream_t;
typedef struct istream { void *ctx; int64_t (*read)(const struct istream *, void *, uint64_t); } istream_t;

typedef struct { plugin_t plugin; double gain; } gain_t;

static const char *const FEATURES[] = {"audio-effect", "stereo", NULL};
static const descriptor_t DESC = {
    {1, 2, 0}, "org.prowave.test-gain", "Test Gain", "Pro Wave", "", "", "", "1.0", "",
    FEATURES,
};
/* An instrument, which the host must leave out of its list. */
static const char *const SYNTH_FEATURES[] = {"instrument", NULL};
static const descriptor_t SYNTH = {
    {1, 2, 0}, "org.prowave.test-synth", "Test Synth", "Pro Wave", "", "", "", "1.0", "",
    SYNTH_FEATURES,
};

static gain_t *self(const plugin_t *p) { return (gain_t *)p->data; }
static bool p_init(const plugin_t *p) { (void)p; return true; }
static void p_destroy(const plugin_t *p) { free(self(p)); }
static bool p_activate(const plugin_t *p, double sr, uint32_t lo, uint32_t hi) {
    (void)p; (void)sr; (void)lo; (void)hi; return true;
}
static void p_deactivate(const plugin_t *p) { (void)p; }
static bool p_start(const plugin_t *p) { (void)p; return true; }
static void p_stop(const plugin_t *p) { (void)p; }
static void p_reset(const plugin_t *p) { (void)p; }
static void p_main(const plugin_t *p) { (void)p; }

static int32_t p_process(const plugin_t *p, const process_t *process) {
    gain_t *g = self(p);
    uint32_t n = process->in_events->size(process->in_events);
    for (uint32_t i = 0; i < n; i++) {
        const event_header_t *h = process->in_events->get(process->in_events, i);
        if (h->space == 0 && h->type == 5 && ((const event_param_t *)h)->param_id == 42)
            g->gain = ((const event_param_t *)h)->value;
    }
    for (uint32_t c = 0; c < 2; c++)
        for (uint32_t i = 0; i < process->frames; i++)
            process->outputs[0].data32[c][i] = (float)(process->inputs[0].data32[c][i] * g->gain);
    return 1;
}

static uint32_t params_count(const plugin_t *p) { (void)p; return 1; }
static bool params_info(const plugin_t *p, uint32_t index, param_info_t *info) {
    (void)p;
    if (index != 0) return false;
    memset(info, 0, sizeof *info);
    info->id = 42;
    strcpy(info->name, "Gain");
    info->min = 0.0;
    info->max = 2.0;
    info->def = 1.0;
    return true;
}
static bool params_value(const plugin_t *p, uint32_t id, double *value) {
    if (id != 42) return false;
    *value = self(p)->gain;
    return true;
}
static bool params_text(const plugin_t *p, uint32_t id, double value, char *out, uint32_t size) {
    (void)p; (void)id;
    if (size < 8) return false;
    strcpy(out, value >= 1.0 ? "loud" : "quiet");
    return true;
}
static bool params_from_text(const plugin_t *p, uint32_t id, const char *text, double *value) {
    (void)p; (void)id; (void)text; (void)value; return false;
}
static void params_flush(const plugin_t *p, const void *in, const void *out) {
    (void)p; (void)in; (void)out;
}
static const struct {
    uint32_t (*count)(const plugin_t *);
    bool (*info)(const plugin_t *, uint32_t, param_info_t *);
    bool (*value)(const plugin_t *, uint32_t, double *);
    bool (*text)(const plugin_t *, uint32_t, double, char *, uint32_t);
    bool (*from_text)(const plugin_t *, uint32_t, const char *, double *);
    void (*flush)(const plugin_t *, const void *, const void *);
} PARAMS = {params_count, params_info, params_value, params_text, params_from_text, params_flush};

static uint32_t ports_count(const plugin_t *p, bool input) { (void)p; (void)input; return 1; }
static bool ports_get(const plugin_t *p, uint32_t index, bool input, port_info_t *info) {
    (void)p; (void)input;
    if (index != 0) return false;
    memset(info, 0, sizeof *info);
    info->channel_count = 2;
    info->flags = 1;
    info->port_type = "stereo";
    info->in_place_pair = UINT32_MAX;
    return true;
}
static const struct {
    uint32_t (*count)(const plugin_t *, bool);
    bool (*get)(const plugin_t *, uint32_t, bool, port_info_t *);
} PORTS = {ports_count, ports_get};

static uint32_t latency_get(const plugin_t *p) { (void)p; return 7; }
static const struct { uint32_t (*get)(const plugin_t *); } LATENCY = {latency_get};

static bool state_save(const plugin_t *p, const ostream_t *s) {
    return s->write(s, &self(p)->gain, sizeof(double)) == (int64_t)sizeof(double);
}
static bool state_load(const plugin_t *p, const istream_t *s) {
    double value;
    if (s->read(s, &value, sizeof value) != (int64_t)sizeof value) return false;
    self(p)->gain = value;
    return true;
}
static const struct {
    bool (*save)(const plugin_t *, const ostream_t *);
    bool (*load)(const plugin_t *, const istream_t *);
} STATE = {state_save, state_load};

static const void *p_extension(const plugin_t *p, const char *id) {
    (void)p;
    if (!strcmp(id, "clap.params")) return &PARAMS;
    if (!strcmp(id, "clap.audio-ports")) return &PORTS;
    if (!strcmp(id, "clap.latency")) return &LATENCY;
    if (!strcmp(id, "clap.state")) return &STATE;
    return NULL;
}

typedef struct factory {
    uint32_t (*count)(const struct factory *);
    const descriptor_t *(*descriptor)(const struct factory *, uint32_t);
    const plugin_t *(*create)(const struct factory *, const host_t *, const char *);
} factory_t;

static uint32_t f_count(const factory_t *f) { (void)f; return 2; }
static const descriptor_t *f_descriptor(const factory_t *f, uint32_t index) {
    (void)f;
    return index == 0 ? &DESC : index == 1 ? &SYNTH : NULL;
}
static const plugin_t *f_create(const factory_t *f, const host_t *host, const char *id) {
    (void)f; (void)host;
    if (strcmp(id, DESC.id)) return NULL;
    gain_t *g = calloc(1, sizeof *g);
    if (!g) return NULL;
    g->gain = 1.0;
    g->plugin = (plugin_t){&DESC, g, p_init, p_destroy, p_activate, p_deactivate, p_start, p_stop,
                           p_reset, p_process, p_extension, p_main};
    return &g->plugin;
}
static const factory_t FACTORY = {f_count, f_descriptor, f_create};

static bool e_init(const char *path) { (void)path; return true; }
static void e_deinit(void) {}
static const void *e_factory(const char *id) {
    return strcmp(id, "clap.plugin-factory") ? NULL : &FACTORY;
}

EXPORT const struct {
    version_t version;
    bool (*init)(const char *);
    void (*deinit)(void);
    const void *(*get_factory)(const char *);
} clap_entry = {{1, 2, 0}, e_init, e_deinit, e_factory};
