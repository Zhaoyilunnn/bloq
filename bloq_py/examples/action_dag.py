"""Analyze the three bit adder's source actions and export their dependencies."""

# [dag-start]
from pathlib import Path

import bloq

source = bloq.GalleryItem.THREE_BIT_ADDER.load()
dag = source.analyze_action_graph()

Path("three-bit-adder-action-dag.svg").write_text(dag.to_svg(), encoding="utf-8")
# [dag-end]
