[[test]]
name = "test_requires_stop"

[[test.input]]
schema_toml = '''
[[fields]]
path = "core_number"
type = "integer"
requires_stop = true

[[fields]]
path = "hostname"
type = "string"
requires_stop = false
'''

[[test.expected]]
field_path = "core_number"
requires_stop = true

[[test.expected]]
field_path = "hostname"
requires_stop = false