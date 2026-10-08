"""Save, load, and convert a small compiled Bloq program."""

# [example-start]
import bloq

program = bloq.compile(bloq.GalleryItem.X_MEMORY.load(), distance=3)
program.save("memory.bloq")
loaded = bloq.Bloq.load("memory.bloq")
loaded.save("memory.bloqir")
# [example-end]

text = program.to_text()
binary = program.to_binary()
from_text = bloq.Bloq.from_text(text)
from_binary = bloq.Bloq.from_binary(binary)
from_file = bloq.Bloq.load("memory.bloqir")
for restored in [loaded, from_text, from_binary, from_file]:
    restored.validate()
    assert restored.to_binary() == binary
    assert restored.to_text() == text
