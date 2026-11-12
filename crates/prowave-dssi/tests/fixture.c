/* Two minimal DSSI plug-ins: a MIDI synth with programs and configuration, and a plain
   generator that only implements LADSPA run(). The ALSA event layout is repeated here because
   the tests must also pass where ALSA headers do not exist (macOS, Windows). */
#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
#define DSSI_EXPORT __declspec(dllexport)
#else
#define DSSI_EXPORT
#endif

typedef float LADSPA_Data;
typedef unsigned long LADSPA_PortDescriptor;
typedef void *LADSPA_Handle;

typedef struct {
    int hint;
    LADSPA_Data lower;
    LADSPA_Data upper;
} LADSPA_PortRangeHint;

typedef struct _LADSPA_Descriptor LADSPA_Descriptor;

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

/* snd_seq_event_t, as alsa/seq_event.h declares it. */
typedef struct {
    unsigned char channel;
    unsigned char note;
    unsigned char velocity;
    unsigned char off_velocity;
    unsigned int duration;
} seq_note;

typedef struct {
    unsigned char channel;
    unsigned char unused[3];
    unsigned int param;
    int value;
} seq_ctrl;

typedef union {
    seq_note note;
    seq_ctrl control;
    unsigned char raw[12];
} seq_data;

typedef union {
    unsigned int tick;
    struct {
        unsigned int tv_sec;
        unsigned int tv_nsec;
    } time;
} seq_timestamp;

typedef struct {
    unsigned char client;
    unsigned char port;
} seq_addr;

typedef struct {
    unsigned char type;
    unsigned char flags;
    unsigned char tag;
    unsigned char queue;
    seq_timestamp time;
    seq_addr source;
    seq_addr dest;
    seq_data data;
} seq_event;

typedef char seq_event_size_check[(sizeof(seq_event) == 28) ? 1 : -1];

typedef struct {
    unsigned long bank;
    unsigned long program;
    const char *name;
} DSSI_Program_Descriptor;

typedef struct _DSSI_Descriptor {
    int DSSI_API_Version;
    const LADSPA_Descriptor *LADSPA_Plugin;
    char *(*configure)(LADSPA_Handle, const char *, const char *);
    const DSSI_Program_Descriptor *(*get_program)(LADSPA_Handle, unsigned long);
    void (*select_program)(LADSPA_Handle, unsigned long, unsigned long);
    int (*get_midi_controller_for_port)(LADSPA_Handle, unsigned long);
    void (*run_synth)(LADSPA_Handle, unsigned long, seq_event *, unsigned long);
    void (*run_synth_adding)(LADSPA_Handle, unsigned long, seq_event *, unsigned long);
    void (*run_multiple_synths)(unsigned long, LADSPA_Handle *, unsigned long,
                                seq_event **, unsigned long *);
    void (*run_multiple_synths_adding)(unsigned long, LADSPA_Handle *, unsigned long,
                                       seq_event **, unsigned long *);
} DSSI_Descriptor;

#define PORT_INPUT 0x1u
#define PORT_OUTPUT 0x2u
#define PORT_CONTROL 0x4u
#define PORT_AUDIO 0x8u

#define EVENT_NOTE_ON 6
#define EVENT_NOTE_OFF 7

/* ------------------------------------------------------------------ the synth */

typedef struct {
    LADSPA_Data *output;
    LADSPA_Data *volume;
    LADSPA_Data *last_frame;
    LADSPA_Data *last_note;
    int gate;
    LADSPA_Data level;
} SynthInstance;

static LADSPA_Handle synth_instantiate(const LADSPA_Descriptor *descriptor, unsigned long rate) {
    (void)descriptor;
    (void)rate;
    return calloc(1, sizeof(SynthInstance));
}

static void synth_connect(LADSPA_Handle handle, unsigned long port, LADSPA_Data *data) {
    SynthInstance *instance = (SynthInstance *)handle;
    if (port == 0) instance->output = data;
    else if (port == 1) instance->volume = data;
    else if (port == 2) instance->last_frame = data;
    else if (port == 3) instance->last_note = data;
}

static void synth_activate(LADSPA_Handle handle) {
    SynthInstance *instance = (SynthInstance *)handle;
    instance->gate = 0;
    instance->level = 0;
}

static void synth_run_synth(LADSPA_Handle handle, unsigned long frames, seq_event *events,
                            unsigned long count) {
    SynthInstance *instance = (SynthInstance *)handle;
    unsigned long i;
    for (i = 0; i < count; ++i) {
        seq_event *event = &events[i];
        if (instance->last_frame) *instance->last_frame = (LADSPA_Data)event->time.tick;
        if (event->type == EVENT_NOTE_ON) {
            if (instance->last_note) *instance->last_note = (LADSPA_Data)event->data.note.note;
            instance->level = (LADSPA_Data)event->data.note.velocity / 127.0f;
            instance->gate = 1;
        } else if (event->type == EVENT_NOTE_OFF) {
            instance->gate = 0;
        }
    }
    if (instance->output) {
        const LADSPA_Data volume = instance->volume ? *instance->volume : 0.0f;
        for (i = 0; i < frames; ++i) {
            instance->output[i] = instance->gate ? instance->level * volume : 0.0f;
        }
    }
}

static void synth_run(LADSPA_Handle handle, unsigned long frames) {
    synth_run_synth(handle, frames, 0, 0);
}

static void synth_deactivate(LADSPA_Handle handle) {
    SynthInstance *instance = (SynthInstance *)handle;
    instance->gate = 0;
}

static void synth_cleanup(LADSPA_Handle handle) {
    free(handle);
}

static char *synth_configure(LADSPA_Handle handle, const char *key, const char *value) {
    (void)handle;
    (void)value;
    if (strcmp(key, "DSSI:TEST") == 0) return 0;
    {
        static const char message[] = "unknown configure key";
        char *error = (char *)malloc(sizeof message);
        if (error) memcpy(error, message, sizeof message);
        return error;
    }
}

static const DSSI_Program_Descriptor synth_programs[] = {
    {0, 0, "Init"},
    {1, 2, "Bright"},
};

static const DSSI_Program_Descriptor *synth_get_program(LADSPA_Handle handle,
                                                         unsigned long index) {
    (void)handle;
    if (index < 2) return &synth_programs[index];
    return 0;
}

static void synth_select_program(LADSPA_Handle handle, unsigned long bank,
                                 unsigned long program) {
    SynthInstance *instance = (SynthInstance *)handle;
    /* DSSI lets a synth rewrite its input control values when a program is selected. */
    if (bank == 1 && program == 2 && instance->volume) *instance->volume = 0.25f;
}

static int synth_midi_controller(LADSPA_Handle handle, unsigned long port) {
    (void)handle;
    /* Volume (port 1) follows MIDI CC 7, encoded as DSSI_CC(7). */
    return port == 1 ? (0x20000000 | 7) : -1;
}

static const LADSPA_PortDescriptor synth_ports[4] = {
    PORT_OUTPUT | PORT_AUDIO,
    PORT_INPUT | PORT_CONTROL,
    PORT_OUTPUT | PORT_CONTROL,
    PORT_OUTPUT | PORT_CONTROL,
};
static const char *const synth_names[4] = {"Out", "Volume", "LastFrame", "LastNote"};
static const LADSPA_PortRangeHint synth_hints[4] = {
    {0, 0.0f, 0.0f},
    {0x1 | 0x2 | 0xc0, 0.0f, 1.0f}, /* bounded below and above, default middle (0.5) */
    {0x1 | 0x2, 0.0f, 1000000000.0f},
    {0x1 | 0x2, 0.0f, 127.0f},
};

static const LADSPA_Descriptor synth_ladspa = {
    7654321u, "dssi_test_synth", 0, "Test Synth", "Pro Wave", "", 4, synth_ports,
    synth_names, synth_hints, 0, synth_instantiate, synth_connect, synth_activate,
    synth_run, 0, 0, synth_deactivate, synth_cleanup,
};

static const DSSI_Descriptor synth_dssi = {
    1, &synth_ladspa, synth_configure, synth_get_program, synth_select_program,
    synth_midi_controller, synth_run_synth, 0, 0, 0,
};

/* ------------------------------------------------------- a generator with no MIDI */

typedef struct {
    LADSPA_Data *output;
} DcInstance;

static LADSPA_Handle dc_instantiate(const LADSPA_Descriptor *descriptor, unsigned long rate) {
    (void)descriptor;
    (void)rate;
    return calloc(1, sizeof(DcInstance));
}

static void dc_connect(LADSPA_Handle handle, unsigned long port, LADSPA_Data *data) {
    DcInstance *instance = (DcInstance *)handle;
    if (port == 0) instance->output = data;
}

static void dc_run(LADSPA_Handle handle, unsigned long frames) {
    DcInstance *instance = (DcInstance *)handle;
    unsigned long i;
    if (!instance->output) return;
    for (i = 0; i < frames; ++i) instance->output[i] = 0.25f;
}

static void dc_cleanup(LADSPA_Handle handle) {
    free(handle);
}

static const LADSPA_PortDescriptor dc_ports[1] = {PORT_OUTPUT | PORT_AUDIO};
static const char *const dc_names[1] = {"Out"};
static const LADSPA_PortRangeHint dc_hints[1] = {{0, 0.0f, 0.0f}};

static const LADSPA_Descriptor dc_ladspa = {
    7654322u, "dssi_test_dc", 0, "Test DC", "Pro Wave", "", 1, dc_ports, dc_names,
    dc_hints, 0, dc_instantiate, dc_connect, 0, dc_run, 0, 0, 0, dc_cleanup,
};

static const DSSI_Descriptor dc_dssi = {
    1, &dc_ladspa, 0, 0, 0, 0, 0, 0, 0, 0,
};

DSSI_EXPORT const DSSI_Descriptor *dssi_descriptor(unsigned long index) {
    if (index == 0) return &synth_dssi;
    if (index == 1) return &dc_dssi;
    return 0;
}
