#!/usr/bin/env python3
"""Persistent Jina ONNX CPU worker; tokenization belongs to the Rust adapter."""
import json
import os
import sys

import numpy as np
import onnxruntime as ort


def pool_and_normalize(hidden):
    if hidden.ndim != 3 or hidden.shape[0] != 1 or hidden.shape[2] != 768:
        raise ValueError("unexpected Jina token embedding shape")
    vector = hidden[0].mean(axis=0, dtype=np.float32)
    norm = np.linalg.norm(vector)
    if not np.isfinite(vector).all() or not np.isfinite(norm) or norm <= 0:
        raise ValueError("invalid Jina embedding")
    return (vector / norm).tolist()


def main():
    # K3 exposes CPU cores 0..7 and AI cores 8..15.
    if hasattr(os, "sched_getaffinity"):
        cpu_cores = os.sched_getaffinity(0) & set(range(8))
        if not cpu_cores:
            raise RuntimeError("no CPU cores available outside the AI core set")
        os.sched_setaffinity(0, cpu_cores)
    options = ort.SessionOptions()
    options.intra_op_num_threads = 4
    options.inter_op_num_threads = 1
    options.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
    session = ort.InferenceSession(sys.argv[1], sess_options=options,
                                   providers=["CPUExecutionProvider"])
    if session.get_providers() != ["CPUExecutionProvider"]:
        raise RuntimeError("embedding must use only the CPU provider")
    inputs = {item.name for item in session.get_inputs()}
    print(json.dumps({"ready": True, "provider": "CPUExecutionProvider", "dimensions": 768}), flush=True)
    for line in sys.stdin:
        try:
            request = json.loads(line)
            batch = request["input_ids"]
            if not 1 <= len(batch) <= 32:
                raise ValueError("batch size must be 1..32")
            vectors = []
            for ids in batch:
                if not 1 <= len(ids) <= 8192:
                    raise ValueError("token count must be 1..8192")
                tokens = np.asarray([ids], dtype=np.int64)
                feed = {"input_ids": tokens, "attention_mask": np.ones_like(tokens),
                        "token_type_ids": np.zeros_like(tokens)}
                result = session.run(None, {key: value for key, value in feed.items() if key in inputs})
                vectors.append(pool_and_normalize(result[0]))
            print(json.dumps({"vectors": vectors}, allow_nan=False), flush=True)
        except Exception as error:
            print(json.dumps({"error": str(error)}), flush=True)


if __name__ == "__main__":
    main()
