// In-process EmbeddingGemma 2 text harness on libllama, shaped like an EmbeddingProvider would be.
// Commands, flags and the measurement protocol are in README.md beside this file.

#include "ggml-backend.h"
#include "llama.h"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <sstream>
#include <sys/mman.h>
#include <string>
#include <thread>
#include <vector>

using clk = std::chrono::steady_clock;

static bool g_quiet = false;
static int g_hold_s = 0;

static long read_kb(const char * path, const char * key) {
    std::ifstream f(path);
    std::string line;
    const size_t klen = strlen(key);
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
    printf("STAGE %s rss_kb=%ld anon_kb=%ld file_kb=%ld memavail_kb=%ld\n", name,
           read_kb("/proc/self/status", "VmRSS:"), read_kb("/proc/self/status", "RssAnon:"),
           read_kb("/proc/self/status", "RssFile:"), read_kb("/proc/meminfo", "MemAvailable:"));
    fflush(stdout);
}

static void log_cb(enum ggml_log_level level, const char * text, void *) {
    // Buffer sizes and the flash-attention decision are the lines a memory budget needs.
    if (level >= GGML_LOG_LEVEL_WARN || (!g_quiet && (strstr(text, "buffer size") || strstr(text, "flash_attn") ||
                                                      strstr(text, "offloaded") || strstr(text, "pooling")))) {
        fputs(text, stderr);
    }
}

// MAP_POPULATE faults in the whole GGUF at load; paging the mapping out and letting the workload
// fault back what it touches turns RSS-file into the encoder's real working set.
static void page_out_model(const std::string & model_path) {
    const std::string base = model_path.substr(model_path.find_last_of('/') + 1);
    std::ifstream     maps("/proc/self/maps");
    std::string       line;
    size_t            bytes = 0;
    while (std::getline(maps, line)) {
        if (line.find(base) == std::string::npos) {
            continue;
        }
        unsigned long lo = 0, hi = 0;
        if (sscanf(line.c_str(), "%lx-%lx", &lo, &hi) == 2 && madvise((void *) lo, hi - lo, 21 /* MADV_PAGEOUT */) == 0) {
            bytes += hi - lo;
        }
    }
    printf("INFO paged_out_mb=%.1f\n", bytes / 1048576.0);
}

static std::vector<std::string> read_lines(const std::string & path) {
    std::ifstream f(path);
    if (!f) {
        fprintf(stderr, "cannot read %s\n", path.c_str());
        exit(1);
    }
    std::vector<std::string> out;
    std::string line;
    while (std::getline(f, line)) {
        if (!line.empty()) {
            out.push_back(line);
        }
    }
    return out;
}

struct Embedder {
    llama_model *       model = nullptr;
    llama_context *     ctx   = nullptr;
    const llama_vocab * vocab = nullptr;
    int                 n_out = 0;
    int                 n_ctx = 0;
    int                 n_seq = 0;

    std::vector<llama_token> tokenize(const std::string & text) const {
        std::vector<llama_token> toks(text.size() + 8);
        int n = llama_tokenize(vocab, text.c_str(), (int) text.size(), toks.data(), (int) toks.size(), true, false);
        if (n < 0) {
            toks.resize(-n);
            n = llama_tokenize(vocab, text.c_str(), (int) text.size(), toks.data(), (int) toks.size(), true, false);
        }
        toks.resize(std::max(n, 0));
        if ((int) toks.size() > n_ctx) {
            toks.resize(n_ctx);
        }
        return toks;
    }

    // Embeds texts[first, first+count) in one llama_encode; returns normalised rows.
    bool embed_batch(const std::vector<std::vector<llama_token>> & toks, size_t first, size_t count,
                     std::vector<float> & out) {
        int n_tokens = 0;
        for (size_t i = 0; i < count; i++) {
            n_tokens += (int) toks[first + i].size();
        }
        llama_batch batch = llama_batch_init(n_tokens, 0, 1);
        int         k     = 0;
        for (size_t s = 0; s < count; s++) {
            const auto & t = toks[first + s];
            for (size_t p = 0; p < t.size(); p++) {
                batch.token[k]     = t[p];
                batch.pos[k]       = (llama_pos) p;
                batch.n_seq_id[k]  = 1;
                batch.seq_id[k][0] = (llama_seq_id) s;
                batch.logits[k]    = 1;
                k++;
            }
        }
        batch.n_tokens = k;
        const int rc   = llama_encode(ctx, batch);
        llama_batch_free(batch);
        if (rc != 0) {
            fprintf(stderr, "llama_encode failed: %d (tokens=%d seqs=%zu)\n", rc, n_tokens, count);
            return false;
        }
        out.resize(count * n_out);
        for (size_t s = 0; s < count; s++) {
            const float * e = llama_get_embeddings_seq(ctx, (llama_seq_id) s);
            if (!e) {
                fprintf(stderr, "no embedding for seq %zu\n", s);
                return false;
            }
            double norm = 0;
            for (int j = 0; j < n_out; j++) {
                norm += (double) e[j] * e[j];
            }
            const float inv = norm > 0 ? (float) (1.0 / std::sqrt(norm)) : 0.0f;
            for (int j = 0; j < n_out; j++) {
                out[s * n_out + j] = e[j] * inv;
            }
        }
        return true;
    }
};

static double pct(std::vector<double> v, double p) {
    std::sort(v.begin(), v.end());
    const double idx = p * (v.size() - 1);
    const size_t lo  = (size_t) idx;
    const size_t hi  = std::min(lo + 1, v.size() - 1);
    return v[lo] + (v[hi] - v[lo]) * (idx - lo);
}

static double ms_since(clk::time_point t0) {
    return std::chrono::duration<double, std::milli>(clk::now() - t0).count();
}

int main(int argc, char ** argv) {
    if (argc < 3) {
        fprintf(stderr,
                "usage: %s MODEL [--dev cpu|gpu] [--threads N] [--ctx N] [--seq N] [--repack 0|1] [--pageout] [--no-host]\n"
                "       [--hold S] [--quiet] vocab | mem | latency Q.txt N | throughput D.txt 1,4,16 R | embed IN OUT\n",
                argv[0]);
        return 2;
    }
    std::string model_path = argv[1];
    std::string dev        = "cpu";
    int         threads    = 4;
    int         n_ctx      = 512;
    int         n_seq      = 16;
    int         repack     = 1;
    bool        pageout    = false;
    bool        no_host    = false;
    int         i          = 2;
    for (; i < argc && strncmp(argv[i], "--", 2) == 0; i++) {
        const std::string a = argv[i];
        if (a == "--quiet") {
            g_quiet = true;
            continue;
        }
        if (a == "--pageout") {
            pageout = true;
            continue;
        }
        if (a == "--no-host") {
            no_host = true;
            continue;
        }
        if (i + 1 >= argc) {
            break;
        }
        if (a == "--dev") {
            dev = argv[++i];
        } else if (a == "--threads") {
            threads = atoi(argv[++i]);
        } else if (a == "--ctx") {
            n_ctx = atoi(argv[++i]);
        } else if (a == "--seq") {
            n_seq = atoi(argv[++i]);
        } else if (a == "--hold") {
            g_hold_s = atoi(argv[++i]);
        } else if (a == "--repack") {
            repack = atoi(argv[++i]);
        }
    }
    if (i >= argc) {
        fprintf(stderr, "missing command\n");
        return 2;
    }
    const std::string cmd = argv[i++];

    llama_log_set(log_cb, nullptr);
    stage("start");

    // Loading only the CPU module keeps libcuda (and a CUDA context) out of a CPU measurement.
    std::string bindir = argv[0];
    bindir             = bindir.substr(0, bindir.find_last_of('/') + 1);
    const char * libdir_env = getenv("EG2_LIBDIR");
    std::string  libdir     = libdir_env ? std::string(libdir_env) + "/" : bindir;
    if (dev == "gpu") {
        ggml_backend_load_all_from_path(libdir.c_str());
    } else {
        ggml_backend_load((libdir + "libggml-cpu.so").c_str());
    }
    llama_backend_init();
    stage("backend");

    llama_model_params mp = llama_model_default_params();
    mp.n_gpu_layers       = dev == "gpu" ? 999 : 0;
    // Repacking speeds the ARM matmuls but holds a second, anonymous copy of the weights.
    mp.use_extra_bufts    = repack != 0;
    mp.vocab_only         = cmd == "vocab";
    // On CUDA the token table otherwise moves into 136 MB of pinned host memory instead of staying mapped,
    // and the automatic load mode stops mapping the file at all.
    mp.no_host            = no_host;
    if (no_host) {
        mp.load_mode = LLAMA_LOAD_MODE_MMAP;
    }
    const auto t_load     = clk::now();
    Embedder   E;
    E.model = llama_model_load_from_file(model_path.c_str(), mp);
    if (!E.model) {
        fprintf(stderr, "model load failed\n");
        return 1;
    }
    const double load_ms = ms_since(t_load);
    stage("model");
    if (cmd == "vocab") {
        llama_model_free(E.model);
        return 0;
    }

    llama_context_params cp = llama_context_default_params();
    cp.n_ctx                = n_ctx;
    cp.n_batch              = n_ctx;
    cp.n_ubatch             = n_ctx;  // a non-causal encode needs each sequence whole in one ubatch
    cp.n_seq_max            = n_seq;
    cp.n_threads            = threads;
    cp.n_threads_batch      = threads;
    cp.embeddings           = true;
    cp.flash_attn_type      = LLAMA_FLASH_ATTN_TYPE_AUTO;
    // Non-unified rounds n_ctx up to 256 per sequence, inflating the compute buffer eightfold.
    cp.kv_unified           = true;
    E.ctx                   = llama_init_from_model(E.model, cp);
    if (!E.ctx) {
        fprintf(stderr, "context init failed\n");
        return 1;
    }
    E.vocab = llama_model_get_vocab(E.model);
    E.n_out = llama_model_n_embd_out(E.model);
    E.n_ctx = n_ctx;
    E.n_seq = n_seq;
    stage("context");
    printf("INFO dev=%s threads=%d ctx=%d seq=%d repack=%d n_out=%d pooling=%d add_bos=%d add_eos=%d load_ms=%.1f\n",
           dev.c_str(), threads, n_ctx, n_seq, repack, E.n_out, (int) llama_pooling_type(E.ctx),
           (int) llama_vocab_get_add_bos(E.vocab), (int) llama_vocab_get_add_eos(E.vocab), load_ms);

    {
        // First encode allocates and warms; it is reported, then excluded from percentiles.
        std::vector<std::vector<llama_token>> w = { E.tokenize("task: search result | query: warm up") };
        std::vector<float>                    v;
        const auto                            t0 = clk::now();
        if (!E.embed_batch(w, 0, 1, v)) {
            return 1;
        }
        printf("INFO first_encode_ms=%.1f tokens=", ms_since(t0));
        for (auto t : w[0]) {
            printf("%d,", t);
        }
        printf("\n");
        stage("warm");
    }
    if (pageout) {
        page_out_model(model_path);
        stage("paged_out");
    }

    if (cmd == "mem") {
        // The "mem" command only reports stages; memwatch.py samples NvMap meanwhile.
    } else if (cmd == "latency" && i + 1 < argc) {
        const auto queries = read_lines(argv[i]);
        const int  n       = atoi(argv[i + 1]);
        std::vector<double> ms;
        std::vector<int>    ntok;
        std::vector<float>  v;
        for (int r = -5; r < n; r++) {
            const std::string & q  = queries[(r + 5) % queries.size()];
            const auto          t0 = clk::now();
            std::vector<std::vector<llama_token>> t = { E.tokenize(q) };
            if (!E.embed_batch(t, 0, 1, v)) {
                return 1;
            }
            if (r >= 0) {
                ms.push_back(ms_since(t0));
                ntok.push_back((int) t[0].size());
            }
        }
        double tok_mean = 0;
        for (int t : ntok) {
            tok_mean += t;
        }
        tok_mean /= ntok.size();
        printf("LATENCY n=%zu tokens_mean=%.1f p50_ms=%.2f p95_ms=%.2f min_ms=%.2f max_ms=%.2f\n", ms.size(), tok_mean,
               pct(ms, 0.5), pct(ms, 0.95), pct(ms, 0.0), pct(ms, 1.0));
        stage("after_latency");
    } else if (cmd == "throughput" && i + 2 < argc) {
        const auto docs = read_lines(argv[i]);
        std::vector<int> sizes;
        {
            std::stringstream ss(argv[i + 1]);
            std::string       tok;
            while (std::getline(ss, tok, ',')) {
                sizes.push_back(atoi(tok.c_str()));
            }
        }
        const int reps = atoi(argv[i + 2]);
        std::vector<std::vector<llama_token>> toks;
        for (const auto & d : docs) {
            toks.push_back(E.tokenize(d));
        }
        std::vector<float> v;
        for (int b : sizes) {
            if (b > n_seq) {
                printf("THROUGHPUT batch=%d skipped (seq=%d)\n", b, n_seq);
                continue;
            }
            std::vector<double> ms;
            size_t              cursor = 0;
            int                 tok_total = 0, batches = 0;
            for (int r = -1; r < reps; r++) {
                // Fill the batch from the corpus, stopping early if the ubatch would overflow.
                size_t count = 0;
                int    ntok  = 0;
                while ((int) count < b) {
                    const auto & t = toks[(cursor + count) % toks.size()];
                    if (ntok + (int) t.size() > n_ctx) {
                        break;
                    }
                    ntok += (int) t.size();
                    count++;
                }
                std::vector<std::vector<llama_token>> slice;
                for (size_t c = 0; c < count; c++) {
                    slice.push_back(toks[(cursor + c) % toks.size()]);
                }
                cursor += count;
                const auto t0 = clk::now();
                if (!E.embed_batch(slice, 0, count, v)) {
                    return 1;
                }
                const double dt = ms_since(t0);
                if (r >= 0) {
                    ms.push_back(dt / count);
                    tok_total += ntok;
                    batches++;
                    if ((int) count != b) {
                        printf("THROUGHPUT note batch=%d filled=%zu (ctx bound)\n", b, count);
                    }
                }
            }
            const double per_item = pct(ms, 0.5);
            printf("THROUGHPUT batch=%d reps=%d tokens_per_batch=%.1f ms_per_item_p50=%.2f batch_ms_p50=%.1f "
                   "ms_per_item_p95=%.2f\n",
                   b, batches, (double) tok_total / std::max(batches, 1), per_item, per_item * b, pct(ms, 0.95));
            fflush(stdout);
        }
        stage("after_throughput");
    } else if (cmd == "embed" && i + 1 < argc) {
        const auto texts = read_lines(argv[i]);
        FILE *     out   = fopen(argv[i + 1], "wb");
        if (!out) {
            fprintf(stderr, "cannot write %s\n", argv[i + 1]);
            return 1;
        }
        std::vector<std::vector<llama_token>> toks;
        for (const auto & t : texts) {
            toks.push_back(E.tokenize(t));
        }
        std::vector<float> v;
        const auto         t0   = clk::now();
        size_t             done = 0;
        while (done < toks.size()) {
            size_t count = 0;
            int    ntok  = 0;
            while (done + count < toks.size() && (int) count < n_seq &&
                   ntok + (int) toks[done + count].size() <= n_ctx) {
                ntok += (int) toks[done + count].size();
                count++;
            }
            if (!E.embed_batch(toks, done, count, v)) {
                return 1;
            }
            for (float x : v) {
                if (!std::isfinite(x)) {
                    fprintf(stderr, "non-finite value in batch at %zu\n", done);
                    return 1;
                }
            }
            fwrite(v.data(), sizeof(float), v.size(), out);
            done += count;
        }
        fclose(out);
        printf("EMBED n=%zu dim=%d ms=%.1f\n", toks.size(), E.n_out, ms_since(t0));
        stage("after_embed");
    } else {
        fprintf(stderr, "unknown or incomplete command %s\n", cmd.c_str());
        return 2;
    }

    llama_free(E.ctx);
    llama_model_free(E.model);
    return 0;
}
