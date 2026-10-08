set -eu
# [example-start]
bloq compile --quiet t-gate.blog -d 11 --backend ir-text -o t-gate.bloqir
bloq stats t-gate.bloqir
# [example-end]
