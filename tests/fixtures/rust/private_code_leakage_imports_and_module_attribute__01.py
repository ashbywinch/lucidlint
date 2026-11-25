from __future__ import annotations

import pkg._internal
import houses.school
import houses.school as school
from houses import _secret
from houses import school as school_module


def read_module_path():
    return houses.school._try_geopoint("51.4", "-1.0")


def read_module_alias():
    return school._try_geopoint("51.4", "-1.0")


def read_from_import():
    return school_module._try_geopoint("51.4", "-1.0")


def public_names_are_fine():
    return houses.school.School, school.School, school_module.School


class Holder:
    """A class reaching its own private attribute is not leakage."""

    def __init__(self):
        self._value = 1

    def read(self):
        return self._value
