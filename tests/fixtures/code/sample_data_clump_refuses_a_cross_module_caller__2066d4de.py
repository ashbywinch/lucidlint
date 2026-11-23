def save(folder, fingerprint, payload):
    return folder + fingerprint + payload


def load(folder, fingerprint):
    return folder + fingerprint


def verify(folder, fingerprint, strict):
    return folder + fingerprint + strict
