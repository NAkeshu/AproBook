const shelf = document.getElementById("book-shelf");
if (!shelf) return;

const outline = shelf.querySelector(".marquee-rect");
let gesture = null;
let previewed = new Set();
let animation = 0;
let suppressClick = false;

function localPoint(clientX, clientY) {
    const bounds = shelf.getBoundingClientRect();
    return {
        x: Math.max(shelf.scrollLeft, Math.min(shelf.scrollLeft + shelf.clientWidth, clientX - bounds.left + shelf.scrollLeft)),
        y: Math.max(shelf.scrollTop, Math.min(shelf.scrollTop + shelf.clientHeight, clientY - bounds.top + shelf.scrollTop)),
    };
}

function updatePreview() {
    if (!gesture || !gesture.dragging) return;
    const end = localPoint(gesture.clientX, gesture.clientY);
    const left = Math.min(gesture.start.x, end.x);
    const top = Math.min(gesture.start.y, end.y);
    const right = Math.max(gesture.start.x, end.x) + 1;
    const bottom = Math.max(gesture.start.y, end.y) + 1;
    outline.style.left = `${left}px`;
    outline.style.top = `${top}px`;
    outline.style.width = `${right - left}px`;
    outline.style.height = `${bottom - top}px`;
    outline.style.display = "block";

    const bounds = shelf.getBoundingClientRect();
    const next = new Set();
    for (const card of shelf.querySelectorAll(".book-card[id^='book-card-']")) {
        const rect = card.getBoundingClientRect();
        const cardLeft = rect.left - bounds.left + shelf.scrollLeft;
        const cardTop = rect.top - bounds.top + shelf.scrollTop;
        if (left < cardLeft + rect.width && right > cardLeft && top < cardTop + rect.height && bottom > cardTop) {
            next.add(card.id.slice("book-card-".length));
            card.classList.add("marquee-preview");
        } else {
            card.classList.remove("marquee-preview");
        }
    }
    previewed = next;
}

function stopGesture() {
    if (!gesture) return;
    gesture = null;
    if (animation) cancelAnimationFrame(animation);
    animation = 0;
    outline.style.display = "none";
    for (const card of shelf.querySelectorAll(".marquee-preview")) card.classList.remove("marquee-preview");
    previewed.clear();
}

function animate() {
    animation = 0;
    if (!gesture || !gesture.dragging) return;
    if (!shelf.isConnected || shelf.classList.contains("marquee-disabled")) {
        stopGesture();
        return;
    }
    const bounds = shelf.getBoundingClientRect();
    const edge = 44;
    let delta = 0;
    if (gesture.clientY < bounds.top + edge) delta = -Math.ceil((bounds.top + edge - gesture.clientY) / 4);
    else if (gesture.clientY > bounds.bottom - edge) delta = Math.ceil((gesture.clientY - bounds.bottom + edge) / 4);
    if (delta) shelf.scrollTop += Math.max(-20, Math.min(20, delta));
    updatePreview();
    if (delta) animation = requestAnimationFrame(animate);
}

function onMouseDown(event) {
    if (gesture || event.button !== 0 || shelf.classList.contains("marquee-disabled")) return;
    if (event.target.closest(".book-card, .empty-state, button, input, select, textarea")) return;
    gesture = {
        start: localPoint(event.clientX, event.clientY),
        originX: event.clientX,
        originY: event.clientY,
        clientX: event.clientX,
        clientY: event.clientY,
        additive: event.metaKey,
        dragging: false,
    };
    event.preventDefault();
}

function onMouseMove(event) {
    if (!gesture) return;
    gesture.clientX = event.clientX;
    gesture.clientY = event.clientY;
    if (!gesture.dragging && Math.hypot(event.clientX - gesture.originX, event.clientY - gesture.originY) > 4) {
        gesture.dragging = true;
        window.getSelection()?.removeAllRanges();
    }
    if (gesture.dragging) {
        event.preventDefault();
        if (!animation) animation = requestAnimationFrame(animate);
    }
}

function onMouseUp(event) {
    if (!gesture) return;
    const dragged = gesture.dragging;
    const additive = gesture.additive;
    if (dragged) {
        gesture.clientX = event.clientX;
        gesture.clientY = event.clientY;
        updatePreview();
        const ids = [...previewed];
        suppressClick = true;
        setTimeout(() => { suppressClick = false; }, 0);
        stopGesture();
        dioxus.send([additive, ids]);
    } else {
        stopGesture();
    }
}

function onClick(event) {
    if (!suppressClick || !shelf.contains(event.target)) return;
    suppressClick = false;
    event.preventDefault();
    event.stopImmediatePropagation();
}

function onKeyDown(event) {
    if (gesture && event.key === "Escape") {
        event.preventDefault();
        stopGesture();
    }
}

function onDragStart(event) {
    if (shelf.contains(event.target)) event.preventDefault();
}

shelf.addEventListener("mousedown", onMouseDown);
window.addEventListener("mousemove", onMouseMove, { passive: false });
window.addEventListener("mouseup", onMouseUp);
window.addEventListener("blur", stopGesture);
window.addEventListener("keydown", onKeyDown);
window.addEventListener("click", onClick, true);
window.addEventListener("dragstart", onDragStart, true);

await new Promise(resolve => {
    const observer = new MutationObserver(() => {
        if (shelf.isConnected) return;
        observer.disconnect();
        stopGesture();
        shelf.removeEventListener("mousedown", onMouseDown);
        window.removeEventListener("mousemove", onMouseMove);
        window.removeEventListener("mouseup", onMouseUp);
        window.removeEventListener("blur", stopGesture);
        window.removeEventListener("keydown", onKeyDown);
        window.removeEventListener("click", onClick, true);
        window.removeEventListener("dragstart", onDragStart, true);
        resolve();
    });
    observer.observe(document.documentElement, { childList: true, subtree: true });
});
