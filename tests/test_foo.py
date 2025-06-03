from docker_layers.foo import foo


def test_foo():
    assert foo("foo") == "foo"
