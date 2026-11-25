/*
 * Test harness that runs the original RCD (rcd_demosaicing.c) outside dcraw to produce reference outputs.
 *
 * rcd_demosaicing.c is written as a dcraw function, so it needs dcraw's globals (width, height, filters,
 * image, etc.) and helpers (FC, LIM, CLIP, merror, border_interpolate). They are defined here with the
 * same semantics as in dcraw. border_interpolate() is copied verbatim from dcraw.c (Dave Coffin).
 *
 * Usage: harness <width> <height> <filters hex> <random|edges> <output file>
 * Output format (little-endian): "RCD1", u32 width, u32 height, u32 filters,
 *   u16 x (width*height) input CFA, u16 x (width*height*3) output RGB (R, G, B per pixel).
 *
 * This program is free software: you can redistribute it and/or modify it under the terms of the
 * GNU General Public License as published by the Free Software Foundation, either version 3 of the
 * License, or (at your option) any later version.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>

typedef unsigned short ushort;
#define CLASS
#define _(s) (s)
#define FORC(cnt) for (c = 0; c < cnt; c++)
#define FORC3 FORC(3)
#define FORCC FORC(colors)
#define MIN(a, b) ((a) < (b) ? (a) : (b))
#define MAX(a, b) ((a) > (b) ? (a) : (b))
#define LIM(x, min, max) MAX(min, MIN(x, max))
#define CLIP(x) LIM((int)(x), 0, 65535)
#define FC(row, col) (filters >> ((((row) << 1 & 14) + ((col) & 1)) << 1) & 3)

ushort width, height;
unsigned filters;
int colors = 3, verbose = 0;
ushort (*image)[4];

static void merror(void *ptr, const char *where)
{
    if (ptr) return;
    fprintf(stderr, "out of memory in %s\n", where);
    exit(1);
}

static int fcol(int row, int col) { return FC(row, col); }

/* border_interpolate from dcraw.c (verbatim) */
void CLASS border_interpolate(int border)
{
    unsigned row, col, y, x, f, c, sum[8];

    for (row = 0; row < height; row++)
        for (col = 0; col < width; col++) {
            if (col == border && row >= border && row < height - border)
                col = width - border;
            memset(sum, 0, sizeof sum);
            for (y = row - 1; y != row + 2; y++)
                for (x = col - 1; x != col + 2; x++)
                    if (y < height && x < width) {
                        f = fcol(y, x);
                        sum[f] += image[y * width + x][f];
                        sum[f + 4]++;
                    }
            f = fcol(row, col);
            FORCC if (c != f && sum[c + 4])
                image[row * width + col][c] = sum[c] / sum[c + 4];
        }
}

/* The original reads before the start of the buffer at row 4 (undefined behavior). A zero-filled margin
 * in front of each allocation makes the result deterministic. */
static void *pcalloc(size_t n, size_t s)
{
    char *p = calloc(n * s + 64, 1);
    return p ? p + 64 : NULL;
}
static void pfree(void *p)
{
    if (p) free((char *)p - 64);
}
#define calloc pcalloc
#define free pfree
#include "rcd_demosaicing.c"
#undef calloc
#undef free

static unsigned state = 12345u;
static unsigned lcg(void)
{
    state = state * 1103515245u + 12345u;
    return state >> 16;
}

/* Test image with edges, a thin line, and a saturated patch. Faint noise is added to break symmetry. */
static unsigned edges_value(int row, int col)
{
    double v = 8000.0 + 30000.0 * col / width;          /* horizontal gradient */
    if (col >= width / 3) v += 12000.0;                 /* vertical edge */
    if (row >= height / 2) v *= 0.6;                    /* horizontal edge */
    if (col + 2 * row > (width + height) * 3 / 4) v += 5000.0; /* diagonal edge */
    if (row == 7) v = 50000.0;                          /* 1-pixel line */
    if (row >= 10 && row < 14 && col >= 30 && col < 34) v = 65535.0; /* saturated patch */
    v += (double)(lcg() % 401) - 200.0;
    if (v < 0) v = 0;
    if (v > 65535) v = 65535;
    return (unsigned)v;
}

static void put_u32(FILE *f, unsigned v)
{
    unsigned char b[4] = {v & 255, (v >> 8) & 255, (v >> 16) & 255, (v >> 24) & 255};
    fwrite(b, 1, 4, f);
}

static void put_u16(FILE *f, unsigned v)
{
    unsigned char b[2] = {v & 255, (v >> 8) & 255};
    fwrite(b, 1, 2, f);
}

int main(int argc, char **argv)
{
    if (argc != 6) {
        fprintf(stderr, "usage: %s <width> <height> <filters hex> <random|edges> <out>\n", argv[0]);
        return 2;
    }
    width = (ushort)atoi(argv[1]);
    height = (ushort)atoi(argv[2]);
    filters = (unsigned)strtoul(argv[3], NULL, 16);
    int use_random = strcmp(argv[4], "random") == 0;
    if (!use_random && strcmp(argv[4], "edges") != 0) {
        fprintf(stderr, "content must be random or edges\n");
        return 2;
    }
    image = calloc((size_t)width * height, sizeof *image);
    ushort *input = calloc((size_t)width * height, sizeof *input);
    merror(image, "main");
    merror(input, "main");
    int row, col;
    for (row = 0; row < height; row++)
        for (col = 0; col < width; col++) {
            unsigned v = use_random ? 1000 + lcg() % 59001 : edges_value(row, col);
            input[row * width + col] = (ushort)v;
            image[row * width + col][FC(row, col)] = (ushort)v;
        }

    rcd_demosaicing();

    FILE *out = fopen(argv[5], "wb");
    if (!out) {
        perror(argv[5]);
        return 1;
    }
    fwrite("RCD1", 1, 4, out);
    put_u32(out, width);
    put_u32(out, height);
    put_u32(out, filters);
    for (int i = 0; i < width * height; i++) put_u16(out, input[i]);
    for (int i = 0; i < width * height; i++)
        for (int c = 0; c < 3; c++) put_u16(out, image[i][c]);
    fclose(out);
    free(input);
    free(image);
    return 0;
}
