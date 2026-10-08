# [example-start]
import bloq

graph = bloq.BlockGraph.load("t-gate.blog")
program = bloq.compile(graph, distance=11)
program.save("t-gate.bloqir")
print(program.stats())
# [example-end]

assert str(bloq.Bloq.load("t-gate.bloqir").stats()) == str(program.stats())
