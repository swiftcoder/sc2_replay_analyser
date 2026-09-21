.PHONY: analyze-replay

# Play a replay through the dockerised SC2 client and emit analysis JSONL.
# Usage: make analyze-replay REPLAY=resources/4656062_tito_puck_MagannathaAIE_v2.SC2Replay
#        make analyze-replay REPLAY=path/to/replay.SC2Replay ANALYZE_ARGS="--maps-dir '/Applications/StarCraft II/Maps' --bot-name puck"
analyze-replay:
	cargo run -- --replay "$(REPLAY)" $(ANALYZE_ARGS)
