/* A stand-in for the part of the FFTW3 API that `dsp` uses, for building the original program
 * where FFTW is not installed (see gen.sh). A plain mixed-radix FFT: slow, but exact to double
 * precision, which is all the golden files need. Part of Pro Wave, GPL-3.0-or-later. */
#ifndef FFTW_SHIM_H
#define FFTW_SHIM_H
#include <stddef.h>
#include <complex.h>

typedef double _Complex fftw_complex;
typedef struct fftw_plan_s *fftw_plan;

#define FFTW_ESTIMATE (1U << 6)
#define FFTW_MEASURE 0U

void *fftw_malloc(size_t n);
void fftw_free(void *p);
fftw_plan fftw_plan_dft_r2c_1d(int n, double *in, fftw_complex *out, unsigned flags);
fftw_plan fftw_plan_dft_c2r_1d(int n, fftw_complex *in, double *out, unsigned flags);
void fftw_execute(const fftw_plan p);
void fftw_execute_dft_r2c(const fftw_plan p, double *in, fftw_complex *out);
void fftw_execute_dft_c2r(const fftw_plan p, fftw_complex *in, double *out);
void fftw_destroy_plan(fftw_plan p);
int fftw_import_wisdom_from_filename(const char *name);
int fftw_export_wisdom_to_filename(const char *name);
#endif
