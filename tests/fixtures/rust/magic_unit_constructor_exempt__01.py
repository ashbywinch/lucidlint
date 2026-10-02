from datetime import timedelta
from pint import Quantity


class Settings:
    max_walk_to_station = Quantity(20, "minute")
    poll_interval = timedelta(minutes=20)