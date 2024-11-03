<script lang="ts">
import {defineComponent, Ref, ref} from 'vue'
import {WsClientConfig} from "../models/WsClientConfig.ts";

export default defineComponent({
  name: "ClientOptionsEditor",
  emits: ['close'],
  props: {
    clientConfig: {
      type: {} as WsClientConfig,
      required: false
    }
  },
  setup(props, {emit}) {
    const clientConfigs = ref<WsClientConfig>({
      name: '',
      listenAddr: '',
      serverAddr: ''
    })
    if (props.clientConfig !== undefined) {
      clientConfigs.value = props.clientConfig;
    }
    const clearView = () => {
      emit('close', clientConfigs);
    }

    return {clearView, clientConfigs}
  }
})
</script>

<template>
  <v-sheet class="mx-auto ma-5" width="300">
    <v-form>
      <v-text-field
          v-model="clientConfigs.name"
          label="Name:">
      </v-text-field>
      <v-text-field
          v-model="clientConfigs.listenAddr"
          label="Listen address:">
      </v-text-field>

      <v-text-field
          v-model="clientConfigs.serverAddr"
          label="Server address:">
      </v-text-field>

      <v-btn @click="clearView()">Save</v-btn>
    </v-form>
  </v-sheet>
</template>

<style scoped>

</style>