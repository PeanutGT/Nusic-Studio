"""Stand-in for resources/ace_worker.py in tests: same JSON-lines protocol, no GPU, no ACE-Step.

Behaviour is picked by the request caption:
    "ok"       phases, writes v1.flac into out_dir, result
    "slow"     emits one diffusing phase, then sleeps (to test cancel)
    "crash"    emits one phase, then exits with code 3
    "error"    emits an error event
"""
import json
import sys
import time
from pathlib import Path


def emit(event, **fields):
    sys.stdout.write(json.dumps({"event": event, **fields}) + "\n")
    sys.stdout.flush()


def main():
    print("fake worker starting (stderr)", file=sys.stderr, flush=True)
    emit("ready", models_loaded=False)
    loaded = False
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        req = json.loads(line)
        op = req.get("op")
        if op == "quit":
            return 0
        if op == "ping":
            emit("ready", models_loaded=loaded, vram_total_gb=4.0, vram_free_gb=3.5)
            continue
        job = req.get("job")
        caption = req.get("caption")
        emit("phase", job=job, name="loading_models", pct=0.02, detail="fake")
        if caption == "crash":
            sys.exit(3)
        if caption == "slow":
            emit("phase", job=job, name="diffusing", pct=0.3, detail="")
            time.sleep(60)
        if caption == "error":
            emit("error", job=job, code="generate", message="boom", hint="try again")
        else:
            emit("warn", job=job, message="fake warning")
            out = Path(req["out_dir"])
            out.mkdir(parents=True, exist_ok=True)
            (out / "v1.flac").write_bytes(b"fLaC")
            emit("phase", job=job, name="writing", pct=0.99, detail="")
            emit("result", job=job, ok=True, warnings=["fake warning"], elapsed_s=0.1,
                 variants=[{"index": 1, "file": "v1.flac", "seed": req["seeds"][0], "sample_rate": 48000}],
                 effective={"dit_model": req["params"]["dit_model"], "device": "cuda", "cpu_decode": False})
            loaded = True
        emit("ready", models_loaded=loaded, model="acestep-v15-turbo" if loaded else None,
             vram_total_gb=4.0, vram_free_gb=3.2)
    return 0


if __name__ == "__main__":
    sys.exit(main())
