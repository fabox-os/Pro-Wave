/* See fftw3.h. */
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include "fftw3.h"

struct fftw_plan_s {
	int n, c2r;
	double *rin, *rout;
	fftw_complex *cin, *cout;
};

void *fftw_malloc(size_t n) { return malloc(n ? n : 1); }
void fftw_free(void *p) { free(p); }
int fftw_import_wisdom_from_filename(const char *name) { (void) name; return 0; }
int fftw_export_wisdom_to_filename(const char *name) { (void) name; return 0; }

/* Decimation in time with the smallest prime factor as radix. `w[k]` = exp(sign*2*pi*i*k/N)
 * for the whole transform of size N; `step` = N/n. */
static void fft_rec(const fftw_complex *in, fftw_complex *out, int n, int stride, const fftw_complex *w, int step)
{
	if (n == 1) { out[0] = in[0]; return; }
	int p = 2;
	while (n % p) ++p;
	const int m = n / p;
	for (int r = 0; r < p; ++r)
		fft_rec(in + (size_t) r * stride, out + (size_t) r * m, m, stride * p, w, step * p);
	fftw_complex *tmp = malloc((size_t) n * sizeof(fftw_complex));
	const int N = n * step;
	for (int k = 0; k < m; ++k) {
		for (int q = 0; q < p; ++q) {
			const int kk = k + q * m;
			fftw_complex s = 0;
			for (int r = 0; r < p; ++r)
				s += out[r * m + k] * w[(size_t) (((long long) r * kk * step) % N)];
			tmp[kk] = s;
		}
	}
	memcpy(out, tmp, (size_t) n * sizeof(fftw_complex));
	free(tmp);
}

static void fft(const fftw_complex *in, fftw_complex *out, int n, int sign)
{
	fftw_complex *w = malloc((size_t) n * sizeof(fftw_complex));
	for (int k = 0; k < n; ++k) {
		const double a = sign * 2.0 * M_PI * k / n;
		w[k] = cos(a) + sin(a) * I;
	}
	fft_rec(in, out, n, 1, w, 1);
	free(w);
}

static void r2c(int n, const double *in, fftw_complex *out)
{
	fftw_complex *x = malloc((size_t) n * sizeof(fftw_complex)), *y = malloc((size_t) n * sizeof(fftw_complex));
	for (int i = 0; i < n; ++i) x[i] = in[i];
	fft(x, y, n, -1);
	memcpy(out, y, (size_t) (n / 2 + 1) * sizeof(fftw_complex));
	free(x); free(y);
}

static void c2r(int n, const fftw_complex *in, double *out)
{
	fftw_complex *x = malloc((size_t) n * sizeof(fftw_complex)), *y = malloc((size_t) n * sizeof(fftw_complex));
	for (int k = 0; k <= n / 2; ++k) x[k] = in[k];
	for (int k = n / 2 + 1; k < n; ++k) x[k] = conj(in[n - k]);
	x[0] = creal(x[0]);
	if (n % 2 == 0) x[n / 2] = creal(x[n / 2]);
	fft(x, y, n, 1);
	for (int i = 0; i < n; ++i) out[i] = creal(y[i]);
	free(x); free(y);
}

fftw_plan fftw_plan_dft_r2c_1d(int n, double *in, fftw_complex *out, unsigned flags)
{
	(void) flags;
	fftw_plan p = calloc(1, sizeof(*p));
	p->n = n; p->rin = in; p->cout = out;
	return p;
}

fftw_plan fftw_plan_dft_c2r_1d(int n, fftw_complex *in, double *out, unsigned flags)
{
	(void) flags;
	fftw_plan p = calloc(1, sizeof(*p));
	p->n = n; p->c2r = 1; p->cin = in; p->rout = out;
	return p;
}

void fftw_execute_dft_r2c(const fftw_plan p, double *in, fftw_complex *out) { r2c(p->n, in, out); }
void fftw_execute_dft_c2r(const fftw_plan p, fftw_complex *in, double *out) { c2r(p->n, in, out); }
void fftw_execute(const fftw_plan p)
{
	if (p->c2r) c2r(p->n, p->cin, p->rout);
	else r2c(p->n, p->rin, p->cout);
}
void fftw_destroy_plan(fftw_plan p) { free(p); }
