# rust-vs-python evaluation report (P7)

Generated: 2026-11-15T12:44:58+00:00  
Schema: `rust-vs-python/1`  
**Verdict: GO**

## Quality (planted-facts battery, rust: n=160 / python: n=160 / baseline: n=160)

| Metric | rust | python | baseline |
|---|---|---|---|
| Precision | 22.81% | 22.81% | 22.79% |
| Recall | 100.00% | 100.00% | 29.88% |
| F1 | 37.15% | 37.15% | 25.86% |
| Exact match | 100.00% | 100.00% | 98.12% |
| Semantic | 59.38% | 59.38% | 0.95% |
| Answer current | 100.00% | 100.00% | 98.12% |
| Citations | 100.00% | 100.00% | 30.63% |
| Token savings vs baseline | 14.28% | 14.28% | n/a |
_match = expected claim in top-5 returned; context bytes method: canonicalized query-prepare JSON payload bytes (context-pack receipt budget_* is a non-stable token estimate)_

Permutation significance (one-sided paired, 20000 perms, seed 20261007):

| Comparison | metric | p(better) | p(worse) | means |
|---|---|---|---|---|
| python_vs_baseline | citation_ok | 0.0 | 1.0 | python=1.000 vs baseline=0.306 |
| python_vs_baseline | context_bytes | 0.0 | 1.0 | python=5418.644 vs baseline=6320.981 |
| python_vs_baseline | correct_current | 0.1264 | 1.0 | python=1.000 vs baseline=0.981 |
| python_vs_baseline | exact_match | 0.1264 | 1.0 | python=1.000 vs baseline=0.981 |
| python_vs_baseline | stale_answer | 0.1264 | 1.0 | python=0.000 vs baseline=0.019 |
| rust_vs_baseline | citation_ok | 0.0 | 1.0 | rust=1.000 vs baseline=0.306 |
| rust_vs_baseline | context_bytes | 0.0 | 1.0 | rust=5418.644 vs baseline=6320.981 |
| rust_vs_baseline | correct_current | 0.1264 | 1.0 | rust=1.000 vs baseline=0.981 |
| rust_vs_baseline | exact_match | 0.1264 | 1.0 | rust=1.000 vs baseline=0.981 |
| rust_vs_baseline | stale_answer | 0.1264 | 1.0 | rust=0.000 vs baseline=0.019 |
| rust_vs_python | citation_ok | 1.0 | 1.0 | rust=1.000 vs python=1.000 |
| rust_vs_python | context_bytes | 1.0 | 1.0 | rust=5418.644 vs python=5418.644 |
| rust_vs_python | correct_current | 1.0 | 1.0 | rust=1.000 vs python=1.000 |
| rust_vs_python | exact_match | 1.0 | 1.0 | rust=1.000 vs python=1.000 |
| rust_vs_python | stale_answer | 1.0 | 1.0 | rust=0.000 vs python=0.000 |

## Memory effectiveness (correction propagation)

| Lane | cases | invalidation rate | stale-answer rate |
|---|---|---|---|
| rust | 3 | 100% | 0% |
| python | 3 | 100% | 0% |
| baseline | 3 | 0% | 100% |

## Performance (medians, k reps — lane B)

```json
{
  "engines": {
    "python": {
      "cold_start": {
        "reps": 20,
        "status_s": {
          "max": 0.061506,
          "median": 0.058063,
          "min": 0.057231,
          "n": 20
        }
      },
      "compile": {
        "reconcile_apply_s": {
          "max": 9.513554,
          "median": 9.290314,
          "min": 9.143089,
          "n": 5
        },
        "reconcile_prepare_s": {
          "max": 8.689517,
          "median": 8.208757,
          "min": 8.117016,
          "n": 5
        },
        "stage_candidates_s": {
          "max": 9.288481,
          "median": 9.219107,
          "min": 8.956194,
          "n": 5
        },
        "total_s": {
          "max": 27.460066,
          "median": 26.718178,
          "min": 26.216299,
          "n": 5
        }
      },
      "graph": {
        "graph_data_s": {
          "max": 0.132841,
          "median": 0.129139,
          "min": 0.127499,
          "n": 5
        },
        "payload_bytes": 174429,
        "viz_serve_ttff_s": {
          "n": 0,
          "reason": "n/a: python engine is the dev-reference CLI; the install target is the rust binary (viz-serve is measured on the shipped engine only)"
        }
      },
      "ingest": {
        "batch_cold_s": {
          "max": 0.616523,
          "median": 0.609273,
          "min": 0.602686,
          "n": 5
        },
        "per_file_s": {
          "max": 0.079851,
          "median": 0.059322,
          "min": 0.055715,
          "n": 750
        },
        "warm_rededup_s": {
          "max": 0.075402,
          "median": 0.068884,
          "min": 0.068604,
          "n": 5
        }
      },
      "name": "python",
      "peak_rss": {
        "ingest_dir_time_l": {
          "max_rss_mb": 32.8,
          "method": "/usr/bin/time -l",
          "wall_s": 0.617
        },
        "pipeline_max_rss_mb": 33.6,
        "pipeline_method": "resource.getrusage(RUSAGE_CHILDREN) peak across all engine subprocesses during the k=5 init+per-file-ingest+stage+prepare+apply pipeline"
      },
      "query": {
        "max_s": 0.078191,
        "median_s": 0.069294,
        "min_s": 0.067767,
        "n": 120,
        "p50_s": 0.069294,
        "p95_s": 0.071766,
        "poisoned_index": {
          "first_query_s": 0.077649,
          "poisoned": [
            {
              "kind": "claims-index-json",
              "path": "/var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/rvsperf-r0damiyr/wiki-python/.llm-wiki/.state/claims-index.json"
            }
          ],
          "rebuild_cost_s": -0.000949,
          "rebuilt_on_query": false,
          "repair_commit_s": 0.261818,
          "repair_commit_steady_s": 0.262768,
          "repair_method": "wall of ingest+stage-candidates+reconcile-prepare+reconcile-apply for one fresh source, poisoned vs healthy wiki",
          "second_query_s": 0.071651,
          "watermarks_after_query": {
            "claims-index.json": 0
          },
          "watermarks_after_repair": {
            "claims-index.json": 304
          },
          "watermarks_before_query": {
            "claims-index.json": 0
          }
        }
      }
    },
    "rust": {
      "cold_start": {
        "reps": 20,
        "status_s": {
          "max": 0.008041,
          "median": 0.00555,
          "min": 0.005383,
          "n": 20
        }
      },
      "compile": {
        "reconcile_apply_s": {
          "max": 1.618481,
          "median": 1.531043,
          "min": 1.497195,
          "n": 5
        },
        "reconcile_prepare_s": {
          "max": 0.433207,
          "median": 0.419,
          "min": 0.414768,
          "n": 5
        },
        "stage_candidates_s": {
          "max": 1.409452,
          "median": 1.388036,
          "min": 1.366113,
          "n": 5
        },
        "total_s": {
          "max": 3.437442,
          "median": 3.361444,
          "min": 3.282309,
          "n": 5
        }
      },
      "graph": {
        "graph_data_s": {
          "max": 0.037358,
          "median": 0.032645,
          "min": 0.032193,
          "n": 5
        },
        "payload_bytes": 160804,
        "viz_serve_ttff_s": {
          "max": 0.061809,
          "median": 0.054382,
          "min": 0.036855,
          "n": 5
        }
      },
      "ingest": {
        "batch_cold_s": {
          "max": 0.546145,
          "median": 0.530419,
          "min": 0.517983,
          "n": 5
        },
        "per_file_s": {
          "max": 0.021328,
          "median": 0.006972,
          "min": 0.006417,
          "n": 750
        },
        "warm_rededup_s": {
          "max": 0.018632,
          "median": 0.015419,
          "min": 0.015016,
          "n": 5
        }
      },
      "name": "rust",
      "peak_rss": {
        "ingest_dir_time_l": {
          "max_rss_mb": 8.0,
          "method": "/usr/bin/time -l",
          "wall_s": 0.549
        },
        "pipeline_max_rss_mb": 34.6,
        "pipeline_method": "resource.getrusage(RUSAGE_CHILDREN) peak across all engine subprocesses during the k=5 init+per-file-ingest+stage+prepare+apply pipeline"
      },
      "query": {
        "max_s": 0.015814,
        "median_s": 0.012282,
        "min_s": 0.011939,
        "n": 120,
        "p50_s": 0.012282,
        "p95_s": 0.013337,
        "poisoned_index": {
          "first_query_s": 0.016016,
          "poisoned": [
            {
              "kind": "rust-sqlite",
              "path": "/var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/rvsperf-r0damiyr/wiki-rust/.llm-wiki/.state/index.sqlite"
            },
            {
              "kind": "claims-index-json",
              "path": "/var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/rvsperf-r0damiyr/wiki-rust/.llm-wiki/.state/claims-index.json"
            }
          ],
          "rebuild_cost_s": 0.225059,
          "rebuilt_on_query": false,
          "repair_commit_s": 0.275122,
          "repair_commit_steady_s": 0.050063,
          "repair_method": "wall of ingest+stage-candidates+reconcile-prepare+reconcile-apply for one fresh source, poisoned vs healthy wiki",
          "second_query_s": 0.011957,
          "watermarks_after_query": {
            "claims-index.json": 0,
            "index.sqlite": 0
          },
          "watermarks_after_repair": {
            "claims-index.json": 304,
            "index.sqlite": 304
          },
          "watermarks_before_query": {
            "claims-index.json": 0,
            "index.sqlite": 0
          }
        }
      }
    }
  },
  "footprint": {
    "note": "python engine also requires a Python >=3.9 runtime on PATH; the rust binary is self-contained",
    "python_engine_bytes": 784054,
    "python_engine_dir": "/Users/harry/orca/workspaces/llm-wiki/orca/skill/llm-wiki/scripts",
    "python_engine_files": 57,
    "rust_binary_bytes": 14026528,
    "rust_binary_stripped_bytes": 12609984
  },
  "headline_speedups": {
    "cold_start_status": 10.462,
    "compile_total": 7.948,
    "graph_data": 3.956,
    "ingest_batch_cold": 1.149,
    "ingest_per_file": 8.509,
    "ingest_warm_dedup": 4.467,
    "query_p50": 5.642,
    "query_p95": 5.381
  },
  "meta": {
    "corpus_dir": "/var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/rvsperf-r0damiyr/corpus",
    "docs": 150,
    "engines": {
      "python": "/opt/homebrew/opt/python@3.15/bin/python3.15 /Users/harry/orca/workspaces/llm-wiki/orca/skill/llm-wiki/scripts/wiki.py",
      "rust": "/Users/harry/orca/workspaces/llm-wiki/orca/rust/target/release/llm-wiki"
    },
    "generated_at": "2026-11-15T12:41:10Z",
    "lane": "rust-vs-python/B",
    "machine": "arm64",
    "platform": "macOS-26.6.2-arm64-arm-64bit-Mach-O",
    "python": "3.15.2",
    "queries": 120,
    "reps": 5
  }
}
```

## Install / UX (lane B)

```json
{
  "binary_install": {
    "binary_installed": true,
    "binary_path": "/var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/rvs-ux-6g9avpw2/install/home/.local/bin/llm-wiki",
    "exit": 0,
    "init_status_ok": true,
    "init_status_s": 0.01,
    "install_s": 0.765,
    "stderr_tail": [],
    "stdout_tail": [
      "verified llm-wiki-local-aarch64-apple-darwin.tar.gz.sha256",
      "installing llm-wiki-local-aarch64-apple-darwin.tar.gz \u2192 /var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/rvs-ux-6g9avpw2/install/home/.local/bin/llm-wiki",
      "installed /var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/rvs-ux-6g9avpw2/install/home/.local/bin/llm-wiki (local)",
      "{\"count\":10,\"installed\":[\"llm-wiki\",\"wiki-compile\",\"wiki-context\",\"wiki-doctor\",\"wiki-ingest\",\"wiki-init\",\"wiki-query\",\"wiki-review\",\"wiki-visualize\",\"wiki-write\"],\"target\":\"/var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/rvs-ux-6g9avpw2/install/home/.omp/agent/skills\"}",
      "",
      "verify with:",
      "  omp -p --no-session 'Do you have skills named llm-wiki and wiki-init? Answer yes/no.'"
    ],
    "step_count": 4,
    "steps": [
      "verified llm-wiki-local-aarch64-apple-darwin.tar.gz.sha256",
      "installing llm-wiki-local-aarch64-apple-darwin.tar.gz \u2192 /var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/rvs-ux-6g9avpw2/install/home/.local/bin/llm-wiki",
      "installed /var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/rvs-ux-6g9avpw2/install/home/.local/bin/llm-wiki (local)",
      "installed 10 skills -> /var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/rvs-ux-6g9avpw2/install/home/.omp/agent/skills"
    ],
    "time_to_working_s": 0.775,
    "wiki_status": {
      "counts": {
        "claims": 0,
        "decisions": 0,
        "pages": 0,
        "pending": 0,
        "sources": 0
      },
      "initialized": true,
      "pending_versions": 0,
      "project": "probe-wiki",
      "revision": 0,
      "skill_version": "0.3.0",
      "stale_artifacts": []
    }
  },
  "failure_modes": {
    "missing_tarball": {
      "clear_error": true,
      "exit": 1,
      "message": [
        "error: LLM_WIKI_BINARY_TARBALL=/var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/rvs-ux-6g9avpw2/nope.tar.gz not found"
      ]
    },
    "python_flag": {
      "dev_reference_message": true,
      "exit": 1,
      "installed_anything": false,
      "message": [
        "error: --python was removed at the phase-6 flip (spec \u00a713).",
        "       The Python engine is now a dev-reference for parity work \u2014",
        "       run it from a repo checkout:",
        "         python3 skill/llm-wiki/scripts/wiki.py <cmd>",
        "       The installed skill surface invokes the llm-wiki binary;",
        "       run ./install.sh (or --binary) to install it."
      ]
    },
    "tampered_sha256": {
      "aborted": true,
      "binary_installed": false,
      "exit": 1,
      "mentions_sha256_mismatch": true,
      "message": [
        "error: sha256 mismatch for llm-wiki-local-aarch64-apple-darwin.tar.gz (want 0214215ee03d6c11bc52526e37ee2f25566b9fd11a33bae9b4c9e325a0121e86, got 3214215ee03d6c11bc52526e37ee2f25566b9fd11a33bae9b4c9e325a0121e86)"
      ]
    }
  },
  "footprint": {
    "note": "python engine also requires a Python >=3.9 runtime on PATH; the rust binary is self-contained",
    "python_engine_bytes": 784054,
    "python_engine_dir": "/Users/harry/orca/workspaces/llm-wiki/orca/skill/llm-wiki/scripts",
    "python_engine_files": 57,
    "rust_binary_bytes": 14010288,
    "rust_binary_stripped_bytes": 12593568
  },
  "iteration_speed": {
    "docs": 150,
    "engines": {
      "python": {
        "max": 0.243579,
        "median": 0.238819,
        "min": 0.237151,
        "n": 5
      },
      "rust": {
        "max": 0.069876,
        "median": 0.069437,
        "min": 0.067271,
        "n": 5
      }
    },
    "method": "edit one claims/*.json value + `compile` + `build-pages` + `query-prepare`, wall per rep"
  },
  "mcp": {
    "client_config_detail": {
      "has_mcpServers": true,
      "keys": [
        "mcpServers"
      ]
    },
    "client_config_path": "/var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/rvs-ux-6g9avpw2/install/home/.claude.json",
    "client_config_written": true,
    "exit": 0,
    "note": "no claude CLI / real client config on sandbox PATH; fallback ~/.claude.json edit exercised; no --dry-run or list-clients flag exists on mcp-register",
    "python_subcommand_count": 32,
    "rust_subcommand_count": 35,
    "stderr_tail": [],
    "stdout_tail": [
      "{\"action\":\"registered\",\"cli_used\":false,\"client\":\"claude-code\",\"config_path\":\"/var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/rvs-ux-6g9avpw2/install/home/.claude.json\",\"config_written\":true,\"docs\":{\"AGENTS.md\":\"created\",\"CLAUDE.md\":\"created\"}}"
    ]
  },
  "meta": {
    "docs": 150,
    "generated_at": "2026-11-15T08:40:23Z",
    "lane": "rust-vs-python/B-install",
    "perf_json": "/Users/harry/orca/workspaces/llm-wiki/orca/evals/rust-vs-python/perf.json",
    "platform": "macOS-26.6.2-arm64-arm-64bit-Mach-O",
    "reps": 5,
    "sandbox_root": "/var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/rvs-ux-6g9avpw2"
  },
  "otool_L": "/Users/harry/orca/workspaces/llm-wiki/orca/rust/target/release/llm-wiki:\n\t/usr/lib/libiconv.2.dylib (compatibility version 7.0.0, current version 7.0.0)\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1356.0.0)\n",
  "scorecard": {
    "faster": {
      "evidence": "python/rust median ratios from perf.json headline speedups",
      "speedups": {
        "cold_start_status": 10.343,
        "compile_total": 8.448,
        "graph_data": 3.966,
        "ingest_batch_cold": 1.13,
        "ingest_per_file": 8.719,
        "ingest_warm_dedup": 4.49,
        "query_p50": 5.54,
        "query_p95": 5.158
      },
      "verdict": "pass"
    },
    "one_command_install": {
      "evidence": "./install.sh --binary (or curl|bash) \u2192 verified llm-wiki on PATH",
      "step_log": [
        "verified llm-wiki-local-aarch64-apple-darwin.tar.gz.sha256",
        "installing llm-wiki-local-aarch64-apple-darwin.tar.gz \u2192 /var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/rvs-ux-6g9avpw2/install/home/.local/bin/llm-wiki",
        "installed /var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/rvs-ux-6g9avpw2/install/home/.local/bin/llm-wiki (local)",
        "installed 10 skills -> /var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/rvs-ux-6g9avpw2/install/home/.omp/agent/skills"
      ],
      "steps": 4,
      "time_to_working_s": 0.775,
      "verdict": "pass"
    },
    "single_binary": {
      "evidence": "rust/target/release/llm-wiki = 14010288 bytes, no runtime deps required",
      "verdict": "pass"
    },
    "smaller": {
      "evidence": "rust binary 14010288 bytes (stripped 12593568) vs python engine source 784054 bytes across 57 files; python additionally requires a >=3.9 interpreter on PATH",
      "verdict": "mixed"
    },
    "zero_external_deps": {
      "evidence": "otool -L lists only system frameworks/libraries",
      "non_system_libs": [],
      "verdict": "pass"
    }
  },
  "tarball": {
    "bytes": 5570808,
    "path": "llm-wiki-local-aarch64-apple-darwin.tar.gz",
    "sha256_sidecar": "llm-wiki-local-aarch64-apple-darwin.tar.gz.sha256"
  }
}
```

## Verdict checks

| Check | Status | Detail |
|---|---|---|
| quality_f1_parity | pass | rust-python F1 delta +0.00pp, p(rust worse)=1.000 |
| quality_exact_match_pct | pass | delta +0.00pp |
| quality_mean_semantic | pass | delta +0.00pp |
| quality_citation_coverage_pct | pass | delta +0.00pp |
| quality_token_savings | pass | delta +0.00pp |
| speed_ingest | pass | python/rust = 8.51x (rust 0.007s vs python 0.059s) |
| speed_compile | pass | python/rust = 7.95x (rust 3.361s vs python 26.718s) |
| speed_query | pass | python/rust = 5.64x (rust 0.012s vs python 0.069s) |
| install_time_to_working | pass | 0.8s |

no waiver needed.
