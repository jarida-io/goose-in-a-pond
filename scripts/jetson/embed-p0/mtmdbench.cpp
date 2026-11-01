// One media encoder's memory and time on libmtmd, without the text transformer: the text GGUF
// loads vocab-only because mtmd needs its tokenizer and hparams, so later stages are the encoder.

#include "llama.h"
#include "mtmd-helper.h"
#include "mtmd.h"

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <string>
#include <thread>
#include <vector>

using clk = std::chrono::steady_clock;
static int g_hold_s = 0;

static long read_kb(const char * path, const char * key) {
    std::ifstream f(path);
    std::string   line;
    const size_t  klen = strlen(key);
    while (std::getline(f, line)) {
        if (line.compare(0, klen, key) == 0) {
            return atol(line.c_str() + klen);
        }
    }
    return -1;
}

static void stage(const char * name) {
    if (g_hold_s > 0) {
        printf("HOLD %s\n", name);
        fflush(stdout);
        std::this_thread::sleep_for(std::chrono::seconds(g_hold_s));
    }
    printf("STAGE %s rss_kb=%ld anon_kb=%ld memavail_kb=%ld\n", name, read_kb("/proc/self/status", "VmRSS:"),
           read_kb("/proc/self/status", "RssAnon:"), read_kb("/proc/meminfo", "MemAvailable:"));
    fflush(stdout);
}

static void log_cb(enum ggml_log_level level, const char * text, void *) {
    if (level >= GGML_LOG_LEVEL_WARN || strstr(text, "buffer size") || strstr(text, "flash")) {
        fputs(text, stderr);
    }
}

int main(int argc, char ** argv) {
    if (argc < 4) {
        fprintf(stderr, "usage: %s TEXT.gguf MMPROJ.gguf [--gpu] [--threads N] [--max-tokens N] [--hold S] FILE...\n",
                argv[0]);
        return 2;
    }
    bool                     gpu = false;
    int                      threads = 4, max_tokens = -1;
    std::vector<std::string> files;
    for (int i = 3; i < argc; i++) {
        const std::string a = argv[i];
        if (a == "--gpu") {
            gpu = true;
        } else if (a == "--threads" && i + 1 < argc) {
            threads = atoi(argv[++i]);
        } else if (a == "--max-tokens" && i + 1 < argc) {
            max_tokens = atoi(argv[++i]);
        } else if (a == "--hold" && i + 1 < argc) {
            g_hold_s = atoi(argv[++i]);
        } else {
            files.push_back(a);
        }
    }
    llama_log_set(log_cb, nullptr);
    mtmd_helper_log_set(log_cb, nullptr);
    stage("start");
    std::string bindir = argv[0];
    bindir             = bindir.substr(0, bindir.find_last_of('/') + 1);
    if (gpu) {
        ggml_backend_load_all_from_path(bindir.c_str());
    } else {
        ggml_backend_load((bindir + "libggml-cpu.so").c_str());
    }
    llama_backend_init();

    llama_model_params mp = llama_model_default_params();
    mp.vocab_only         = true;
    llama_model * model   = llama_model_load_from_file(argv[1], mp);
    if (!model) {
        fprintf(stderr, "text model load failed\n");
        return 1;
    }
    stage("vocab");

    mtmd_context_params cp = mtmd_context_params_default();
    cp.use_gpu             = gpu;
    cp.n_threads           = threads;
    cp.print_timings       = false;
    cp.warmup              = false;
    cp.flash_attn_type     = LLAMA_FLASH_ATTN_TYPE_AUTO;
    if (max_tokens > 0) {
        cp.image_max_tokens = max_tokens;
    }
    const auto     t0  = clk::now();
    mtmd_context * ctx = mtmd_init_from_file(argv[2], model, cp);
    if (!ctx) {
        fprintf(stderr, "mtmd init failed\n");
        return 1;
    }
    printf("INFO encoder_load_ms=%.0f vision=%d audio=%d\n",
           std::chrono::duration<double, std::milli>(clk::now() - t0).count(), (int) mtmd_support_vision(ctx),
           (int) mtmd_support_audio(ctx));
    stage("encoder");

    std::vector<double> ms;
    for (size_t f = 0; f < files.size(); f++) {
        auto wrap = mtmd_helper_bitmap_init_from_file(ctx, files[f].c_str(), false, mtmd_helper_init_opt_default());
        if (!wrap.bitmap) {
            fprintf(stderr, "cannot read %s\n", files[f].c_str());
            return 1;
        }
        mtmd_input_chunks * chunks = mtmd_input_chunks_init();
        mtmd_input_text     text   = { mtmd_default_marker(), strlen(mtmd_default_marker()), true, true };
        const mtmd_bitmap * bm     = wrap.bitmap;
        const auto          t1     = clk::now();
        if (mtmd_tokenize(ctx, chunks, &text, &bm, 1) != 0) {
            fprintf(stderr, "tokenize failed for %s\n", files[f].c_str());
            return 1;
        }
        size_t n_tok = 0;
        for (size_t c = 0; c < mtmd_input_chunks_size(chunks); c++) {
            const mtmd_input_chunk * ch = mtmd_input_chunks_get(chunks, c);
            if (mtmd_input_chunk_get_type(ch) != MTMD_INPUT_CHUNK_TYPE_TEXT) {
                if (mtmd_encode_chunk(ctx, ch) != 0) {
                    fprintf(stderr, "encode failed for %s\n", files[f].c_str());
                    return 1;
                }
                n_tok += mtmd_input_chunk_get_n_tokens(ch);
            }
        }
        const double dt = std::chrono::duration<double, std::milli>(clk::now() - t1).count();
        ms.push_back(dt);
        printf("MEDIA %s soft_tokens=%zu ms=%.1f\n", files[f].c_str(), n_tok, dt);
        fflush(stdout);
        mtmd_input_chunks_free(chunks);
        mtmd_bitmap_free(wrap.bitmap);
    }
    if (ms.size() > 1) {
        std::vector<double> rest(ms.begin() + 1, ms.end());
        std::sort(rest.begin(), rest.end());
        printf("ENCODE n=%zu first_ms=%.1f p50_ms=%.1f max_ms=%.1f\n", ms.size(), ms[0], rest[rest.size() / 2],
               rest.back());
    }
    stage("after_encode");
    mtmd_free(ctx);
    llama_model_free(model);
    return 0;
}
